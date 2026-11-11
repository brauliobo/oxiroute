#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn compile_forward_proxy_services(
    services: &[crate::forward_proxy::ForwardServiceBlueprint],
    cache_specs: &[CacheStoreBlueprint],
    mode: AcquisitionMode,
    deadline: Option<Instant>,
) -> Result<Vec<Arc<ForwardHttp1ServicePlan>>, RuntimeAcquisitionError> {
    let mut cache_backends = HashMap::new();
    services
        .iter()
        .map(|service| {
            let cache = acquire_cache_policy(
                &service.name,
                0,
                service.cache.as_ref(),
                cache_specs,
                &mut cache_backends,
                mode,
                deadline,
            )?;
            let plan =
                ForwardHttp1ServicePlan::acquire(service.clone(), cache).map_err(|source| {
                    ServicePlanError::ForwardProxyPreflight {
                        service: service.name.clone(),
                        source,
                    }
                })?;
            Ok(Arc::new(plan))
        })
        .collect()
}

pub(super) struct CompiledPools {
    pub(super) plans: Vec<Arc<UpstreamPlan>>,
    pub(super) health_groups: Vec<health::HealthGroup>,
    pub(super) canaries: Vec<health::CanaryRunner>,
    pub(super) ordered: Vec<Arc<RoundRobinPool>>,
}

#[expect(
    clippy::too_many_lines,
    reason = "pool compilation performs one atomic validation and construction pass"
)]
pub(super) fn compile_pools(
    pool_specs: &[PoolBlueprint],
    protected_addresses: &Arc<[std::net::SocketAddr]>,
    passive_policy_override: Option<PassiveFailurePolicy>,
) -> Result<CompiledPools, ServicePlanError> {
    let mut pools = HashMap::with_capacity(pool_specs.len());
    let mut health_groups = Vec::new();
    let mut canaries = Vec::new();
    let mut ordered = Vec::with_capacity(pool_specs.len());
    let mut plans = Vec::with_capacity(pool_specs.len());
    for pool in pool_specs {
        let passive_policy = passive_policy_override.unwrap_or(pool.passive_health);
        let servers = pool
            .endpoints
            .iter()
            .map(|server| {
                let endpoint = server.endpoint.clone();
                let pinned_addresses: Option<Arc<[std::net::SocketAddr]>> =
                    if let Some((host, port)) = &server.startup_dns {
                        let addresses =
                            (host.as_str(), *port).to_socket_addrs().map_err(|error| {
                                PoolError::StartupDns {
                                    server: server.name.clone(),
                                    detail: error.to_string(),
                                }
                            })?;
                        Some(
                            endpoint
                                .order_addresses(addresses)
                                .map_err(|error| PoolError::StartupDns {
                                    server: server.name.clone(),
                                    detail: error.to_string(),
                                })?
                                .into(),
                        )
                    } else {
                        None
                    };
                let startup_addresses = match (&server.endpoint, &pinned_addresses) {
                    (RuntimeEndpoint::Socket { address }, _) => std::slice::from_ref(address),
                    (_, Some(addresses)) => addresses.as_ref(),
                    _ => &[],
                };
                if startup_addresses.iter().any(|address| {
                    protected_addresses.iter().any(|protected| {
                        address.port() == protected.port()
                            && (address.ip() == protected.ip() || protected.ip().is_unspecified())
                    })
                }) {
                    return Err(PoolError::ProtectedEndpoint {
                        server: server.name.clone(),
                    });
                }
                Ok(RuntimeServer {
                    name: server.name.clone(),
                    endpoint,
                    max_connections: server.max_connections,
                    pinned_addresses,
                    protected_addresses: Arc::clone(protected_addresses),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            })?;
        let selector = Arc::new(RoundRobinPool::acquire_compiled(
            pool.name.clone(),
            servers,
            pool.health.as_ref().map(|health| health.startup),
            pool.queue_timeout,
            passive_policy,
            pool.resilience,
            &pool.construction,
        ));
        let tls = pool
            .upstream_tls
            .as_ref()
            .map(crate::tls::UpstreamTlsPlan::acquire)
            .transpose()
            .map_err(|source| ServicePlanError::Tls(Box::new(source)))?
            .map(Arc::new);
        let h3 = pool
            .upstream_tls
            .as_ref()
            .map_or(Ok(None), crate::H3UpstreamPlan::acquire)
            .map_err(|source| ServicePlanError::H3Upstream {
                pool: pool.name.clone(),
                source: Box::new(source),
            })?
            .map(Arc::new);
        let compiled = Arc::new(
            UpstreamPlan::with_http_policy(
                Arc::clone(&selector),
                tls,
                pool.connect_timeout,
                pool.server_timeout,
                pool.connection_reuse,
                pool.min_http_version,
                h3,
            )
            .with_unavailable_response(pool.unavailable.clone()),
        );
        if let Some(health_check) = &pool.health {
            health_groups.push(health::acquire_health_group(
                &pool.name,
                &selector,
                health_check,
            ));
        }
        if let Some(canary) = &pool.canary {
            canaries.push(health::CanaryRunner::new(Arc::clone(&selector), canary));
        }
        pools.insert(pool.name.clone(), Arc::clone(&compiled));
        plans.push(compiled);
        ordered.push(selector);
    }
    Ok(CompiledPools {
        plans,
        health_groups,
        canaries,
        ordered,
    })
}

pub(super) fn compile_l4_services(
    services: &[L4ServiceBlueprint],
    pools: &[Arc<UpstreamPlan>],
) -> Vec<Arc<L4ServicePlan>> {
    let mut l4_services = Vec::with_capacity(services.len());
    for service in services {
        let pool = &pools[service.pool];
        l4_services.push(Arc::new(L4ServicePlan::new(
            RelayPolicy {
                connect: pool.connect_timeout(service.connect_timeout),
                idle: Some(service.idle_timeout),
                lifetime: service.lifetime_timeout,
            },
            Arc::clone(pool.selector()),
            service.proxy_protocol,
            service.udp,
        )));
    }
    l4_services
}

#[allow(clippy::too_many_lines)]
#[cfg(test)]
pub(crate) fn compile_rtmp_value_plans(
    config: &ValidatedConfig,
) -> Result<Vec<oxiroute_rtmp::RtmpServicePlan>, oxiroute_rtmp::RtmpPrepareError> {
    crate::rtmp::value_plan::compile_rtmp_value_plans_from_draft(config.as_draft())
}

pub(super) fn compile_rtmp_services(
    specs: &[RtmpSpec],
    mode: AcquisitionMode,
) -> Result<Vec<Arc<RtmpServicePlan>>, ServicePlanError> {
    specs
        .iter()
        .map(|spec| {
            Ok(Arc::new(RtmpServicePlan {
                value_plan: spec.plan.clone(),
                access_log: acquire_access_log(
                    spec.plan.service_id(),
                    spec.access_log.as_ref(),
                    true,
                    mode,
                )?,
            }))
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
pub(super) fn compile_listener(
    listener: &ListenerBlueprint,
    http_services: &[Arc<HttpServicePlan>],
    forward_services: &[Arc<ForwardHttp1ServicePlan>],
    rtmp_services: &[Arc<RtmpServicePlan>],
    l4_services: &[Arc<L4ServicePlan>],
    tls_profiles: &crate::tls::TlsProfilePlanMap,
    tls_blueprint: &crate::tls::TlsBlueprint,
) -> ServiceSpec {
    let tls = listener.tls_profile.map(|index| {
        Arc::clone(
            tls_profiles
                .get(&tls_blueprint.profiles[index].name)
                .expect("compiled TLS profile identity"),
        )
    });
    let kind = match (listener.protocol, listener.service) {
        (Protocol::Http, ServiceReference::Http(index)) => {
            ServiceKind::Http(Arc::clone(&http_services[index]))
        }
        (Protocol::Http3, ServiceReference::Http(index)) => {
            ServiceKind::Http3(Arc::clone(&http_services[index]))
        }
        (Protocol::ForwardHttp1, ServiceReference::Forward(index)) => {
            ServiceKind::ForwardHttp1(Arc::clone(&forward_services[index]))
        }
        (Protocol::ForwardHttp2, ServiceReference::Forward(index)) => {
            ServiceKind::ForwardHttp2(Arc::clone(&forward_services[index]))
        }
        (Protocol::ForwardHttp3, ServiceReference::Forward(index)) => {
            ServiceKind::ForwardHttp3(Arc::clone(&forward_services[index]))
        }
        (Protocol::Rtmp, ServiceReference::Rtmp(index)) => {
            ServiceKind::Rtmp(Arc::clone(&rtmp_services[index]))
        }
        (Protocol::Tcp, ServiceReference::L4(index)) => {
            ServiceKind::Tcp(Arc::clone(&l4_services[index]))
        }
        (Protocol::Udp, ServiceReference::L4(index)) => {
            ServiceKind::Udp(Arc::clone(&l4_services[index]))
        }
        _ => unreachable!("compiled listener protocol identity"),
    };
    ServiceSpec {
        name: listener.name.clone(),
        bind: listener.bind.clone(),
        max_connections: listener.max_connections,
        downstream_timeouts: listener.downstream_timeouts,
        h2_windows: listener.h2_windows,
        proxy_protocol: listener.proxy_protocol,
        kind,
        tls,
    }
}
