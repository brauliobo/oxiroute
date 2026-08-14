use std::{sync::Arc, time::Duration};

use http::{HeaderName, Method, StatusCode};
use oxiroute_cache::{CacheConfig, CacheTimeline, DiskCacheConfig};

use oxiroute_config::{
    CacheAuthorizationPolicy, CacheKeyComponent, CacheSetCookiePolicy, CacheStore, CacheVaryPolicy,
    DnsResolutionPolicy, HttpProxyPolicy, HttpRouteAction, HttpVersion, Protocol, ValidatedConfig,
};
use oxiroute_rtmp::{RtmpCapabilities, RtmpServicePlan};

use crate::{
    PassiveFailurePolicy, Route, RouteTable, TopologySnapshot,
    health::HealthCheckBlueprint,
    http_action::{
        FixedResponsePlan, HttpGzipPlan, ProxyPolicyPlan, RedirectPlan, RoutePolicyPlan,
        StaticFilesBlueprint,
    },
    planning_errors::{ServicePlanError, rtmp_preparation_error},
    planning_types::{
        CachePolicyBlueprint, CacheStoreBlueprint, EndpointBlueprint, HttpActionBlueprint,
        HttpRouteBlueprint, HttpServiceBlueprint, L4ServiceBlueprint, ListenerBlueprint,
        PoolBlueprint, RtmpSpec, ServiceReference,
    },
    rtmp_value_plan::compile_rtmp_value_plans_from_draft,
    runtime_policy::reject_unimplemented_runtime_policies,
};

/// Immutable, value-only decisions for one validated generation.
pub(crate) struct GenerationBlueprint {
    pub(crate) max_connections: Option<u64>,
    pub(crate) protected_addresses: Arc<[std::net::SocketAddr]>,
    pub(crate) listener_specs: Result<Box<[ListenerBlueprint]>, ServicePlanError>,
    pub(crate) pool_specs: Result<Box<[PoolBlueprint]>, ServicePlanError>,
    pub(crate) cache_specs: Result<Box<[CacheStoreBlueprint]>, ServicePlanError>,
    pub(crate) http_service_specs: Result<Box<[HttpServiceBlueprint]>, ServicePlanError>,
    pub(crate) forward_service_specs:
        Result<Box<[crate::forward_proxy::ForwardServiceBlueprint]>, ServicePlanError>,
    pub(crate) l4_service_specs: Result<Box<[L4ServiceBlueprint]>, ServicePlanError>,
    pub(crate) tls: crate::tls::TlsBlueprint,
    pub(crate) rtmp_specs: Result<Box<[RtmpSpec]>, ServicePlanError>,
    pub(crate) rtmp_capabilities: RtmpCapabilities,
    pub(crate) rtmp_recording_supported: bool,
    pub(crate) topology: Arc<TopologySnapshot>,
}

pub(crate) struct GenerationCompiler;

impl GenerationCompiler {
    pub(crate) fn compile(
        validated: &ValidatedConfig,
    ) -> Result<GenerationBlueprint, ServicePlanError> {
        let config = validated.as_draft();
        reject_unimplemented_runtime_policies(config)?;
        let pool_specs = compile_pool_blueprints(config);
        let cache_specs = compile_cache_store_blueprints(config);
        let http_service_specs = compile_http_blueprints(config);
        let forward_service_specs = compile_forward_blueprints(config);
        let rtmp_value_plans = compile_rtmp_value_plans_from_draft(config)
            .map_err(rtmp_preparation_error)
            .map(|plans| {
                plans
                    .into_iter()
                    .zip(&config.rtmp_services)
                    .map(|(plan, service)| compile_rtmp_blueprint(plan, service))
                    .collect::<Box<[_]>>()
            });
        let topology = Arc::new(TopologySnapshot::compile(config));
        let active_rtmp_services = config.listeners.iter().filter_map(|listener| {
            (listener.protocol == Protocol::Rtmp)
                .then_some(listener.service.as_deref())
                .flatten()
                .and_then(|name| {
                    config
                        .rtmp_services
                        .iter()
                        .find(|service| service.name == name)
                })
        });
        let rtmp_capabilities = RtmpCapabilities {
            live_ingest: active_rtmp_services.clone().next().is_some(),
            manual_recording: active_rtmp_services.clone().any(|service| {
                service.applications.iter().any(|application| {
                    application.recorders.iter().any(|recorder| {
                        recorder.start == oxiroute_config::RtmpRecorderStart::Manual
                    })
                })
            }),
        };
        let rtmp_recording_supported = active_rtmp_services.clone().any(|service| {
            service
                .applications
                .iter()
                .any(|app| !app.recorders.is_empty())
        });
        Ok(GenerationBlueprint {
            max_connections: config.max_connections,
            protected_addresses: config
                .management
                .iter()
                .map(|management| management.bind)
                .chain(config.stats.iter().flat_map(|stats| {
                    stats
                        .binds
                        .iter()
                        .copied()
                        .chain(stats.pages.iter().map(|page| page.bind))
                }))
                .collect(),
            listener_specs: compile_listener_blueprints(config),
            pool_specs,
            cache_specs,
            http_service_specs,
            forward_service_specs,
            l4_service_specs: compile_l4_blueprints(config),
            tls: crate::tls::TlsBlueprint::compile(config)
                .map_err(|source| ServicePlanError::Tls(Box::new(source)))?,
            rtmp_specs: rtmp_value_plans,
            rtmp_capabilities,
            rtmp_recording_supported,
            topology,
        })
    }
}

fn compile_forward_blueprints(
    config: &oxiroute_config::ConfigDraft,
) -> Result<Box<[crate::forward_proxy::ForwardServiceBlueprint]>, ServicePlanError> {
    config
        .forward_proxy_services
        .iter()
        .map(|service| {
            let cache = service
                .header_policy
                .cache
                .as_deref()
                .map(|policy| {
                    compile_cache_policy_values(config, &service.name, 0, false, false, &[], policy)
                })
                .transpose()?;
            crate::forward_proxy::ForwardServiceBlueprint::compile(service, cache).map_err(
                |source| ServicePlanError::ForwardProxyPreflight {
                    service: service.name.clone(),
                    source,
                },
            )
        })
        .collect()
}

fn compile_pool_blueprints(
    config: &oxiroute_config::ConfigDraft,
) -> Result<Box<[PoolBlueprint]>, ServicePlanError> {
    config
        .upstream_pools
        .iter()
        .map(|pool| {
            let endpoints =
                pool.servers
                    .iter()
                    .map(|server| {
                        let endpoint = crate::RuntimeEndpoint::compile(&server.endpoint).map_err(
                            |source| ServicePlanError::Pool {
                                pool: pool.name.clone(),
                                source,
                            },
                        )?;
                        let startup_dns = (server.dns_resolution == DnsResolutionPolicy::Startup)
                            .then(|| match &server.endpoint {
                                oxiroute_config::UpstreamEndpoint::Dns { host, port } => {
                                    (host.clone(), *port)
                                }
                                _ => unreachable!("validated startup DNS endpoint"),
                            });
                        Ok(EndpointBlueprint {
                            name: server.name.clone(),
                            endpoint,
                            startup_dns,
                            max_connections: server.max_connections,
                        })
                    })
                    .collect::<Result<Box<[_]>, ServicePlanError>>()?;
            let passive_health = pool
                .passive_health
                .as_ref()
                .map(PassiveFailurePolicy::from_config)
                .unwrap_or_default();
            let health = pool
                .health_check
                .as_ref()
                .map(|health| HealthCheckBlueprint::compile(&pool.name, health))
                .transpose()
                .map_err(|source| ServicePlanError::Health {
                    pool: pool.name.clone(),
                    source: Box::new(source),
                })?;
            let upstream_tls = crate::tls::UpstreamTlsBlueprint::compile(
                &pool.name,
                pool.tls.as_ref(),
                pool.http_versions,
            )
            .map_err(|source| ServicePlanError::Tls(Box::new(source)))?;
            if pool.http_versions.min == HttpVersion::Http3 && upstream_tls.is_none() {
                return Err(ServicePlanError::H3Upstream {
                    pool: pool.name.clone(),
                    source: Box::new(crate::H3UpstreamBuildError::EmptyRoots {
                        pool: pool.name.clone(),
                    }),
                });
            }
            crate::PassiveFailurePolicy::validate(passive_health).map_err(|source| {
                ServicePlanError::Pool {
                    pool: pool.name.clone(),
                    source,
                }
            })?;
            let construction = crate::routing::PoolConstructionBlueprint::compile(
                &pool.algorithm,
                endpoints.len(),
            )
            .map_err(|source| ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            })?;
            Ok(PoolBlueprint {
                name: pool.name.clone(),
                endpoints,
                health,
                passive_health,
                upstream_tls,
                min_http_version: pool.http_versions.min,
                queue_timeout: pool.queue_timeout_ms.map(Duration::from_millis),
                connect_timeout: pool.connect_timeout_ms.map(Duration::from_millis),
                server_timeout: pool.server_timeout_ms.map(Duration::from_millis),
                connection_reuse: pool.connection_reuse,
                construction,
            })
        })
        .collect()
}

fn compile_cache_store_blueprints(
    config: &oxiroute_config::ConfigDraft,
) -> Result<Box<[CacheStoreBlueprint]>, ServicePlanError> {
    config
        .cache_stores
        .iter()
        .map(compile_cache_store)
        .collect()
}

fn compile_cache_store(store: &CacheStore) -> Result<CacheStoreBlueprint, ServicePlanError> {
    let common = store.common();
    let unavailable = || ServicePlanError::RuntimePolicyUnavailable {
        policy: "cache bounds",
    };
    let to_usize = |value| usize::try_from(value).map_err(|_| unavailable());
    let memory = CacheConfig {
        max_entries: to_usize(common.max_entries)?,
        max_total_bytes: to_usize(common.max_bytes)?,
        max_object_bytes: to_usize(common.max_object_bytes)?,
        max_header_bytes: to_usize(common.max_header_bytes)?,
        max_header_fields: 256,
        max_body_bytes: to_usize(common.max_object_bytes)?,
        max_key_bytes: to_usize(common.max_key_bytes)?,
        max_vary_fields: 32,
        max_tags_per_entry: to_usize(common.max_tags_per_object)?,
        max_tag_bytes: to_usize(common.max_tag_bytes)?,
        max_in_flight: to_usize(common.max_in_flight_fills)?,
        max_followers_per_fill: to_usize(common.max_followers_per_fill)?,
        max_heuristic_freshness: Duration::from_hours(24),
    };
    Ok(match store {
        CacheStore::Memory { name, .. } => CacheStoreBlueprint::Memory {
            name: name.clone(),
            config: memory,
        },
        CacheStore::Disk {
            name,
            root_directory,
            ..
        } => CacheStoreBlueprint::Disk {
            name: name.clone(),
            root: root_directory.clone(),
            config: DiskCacheConfig {
                memory,
                max_disk_bytes: common.max_bytes,
                max_disk_files: to_usize(common.max_entries)?,
                max_record_bytes: to_usize(common.max_bytes)?,
            },
        },
    })
}

fn compile_http_blueprints(
    config: &oxiroute_config::ConfigDraft,
) -> Result<Box<[HttpServiceBlueprint]>, ServicePlanError> {
    config
        .http_services
        .iter()
        .map(|service| {
            let routes = service
                .routes
                .iter()
                .enumerate()
                .map(|(route_index, route)| {
                    compile_http_route_blueprint(config, service, route_index, route)
                })
                .collect::<Result<Box<[_]>, _>>()?;
            let route_table =
                RouteTable::new(routes.iter().map(|route| route.route.clone()).collect());
            Ok(HttpServiceBlueprint {
                name: service.name.clone(),
                routes,
                automatic_response_headers: service.automatic_response_headers,
                upstream_io_timeout: Duration::from_millis(service.upstream_io_timeout_ms),
                max_request_body_bytes: service.max_request_body_bytes,
                gzip: service.gzip.as_ref().map(HttpGzipPlan::compile),
                access_log: service.access_log.clone(),
                route_table,
            })
        })
        .collect()
}

fn compile_http_route_blueprint(
    config: &oxiroute_config::ConfigDraft,
    service: &oxiroute_config::HttpService,
    route_index: usize,
    route: &oxiroute_config::HttpRoute,
) -> Result<HttpRouteBlueprint, ServicePlanError> {
    let methods = (!route.methods.is_empty())
        .then(|| {
            route
                .methods
                .iter()
                .map(|method| {
                    method
                        .parse::<Method>()
                        .map_err(|_| ServicePlanError::InvalidMethod {
                            service: service.name.clone(),
                            route: route_index,
                            method: method.clone(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let route_id = route_index.to_string();
    let compiled_route = Route::new(route.host.clone(), route.path.clone(), methods, route_id)
        .map_err(|source| ServicePlanError::Route {
            service: service.name.clone(),
            route: route_index,
            source,
        })?;
    let action = match &route.action {
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } => {
            let pool = config
                .upstream_pools
                .iter()
                .position(|pool| pool.name == *upstream_pool)
                .ok_or_else(|| ServicePlanError::UnknownHttpPool {
                    service: service.name.clone(),
                    route: route_index,
                    pool: upstream_pool.clone(),
                })?;
            HttpActionBlueprint::Proxy {
                pool,
                policy: ProxyPolicyPlan::compile(policy),
                cache: compile_cache_policy_blueprint(config, service, route_index, route, policy)?,
            }
        }
        HttpRouteAction::FixedResponse {
            status,
            body,
            headers,
        } => HttpActionBlueprint::Fixed(FixedResponsePlan::compile(*status, body, headers)),
        HttpRouteAction::Redirect {
            status,
            location,
            headers,
        } => HttpActionBlueprint::Redirect(RedirectPlan::compile(*status, location, headers)),
        action @ HttpRouteAction::StaticFiles { .. } => HttpActionBlueprint::Static(
            StaticFilesBlueprint::compile(compiled_route.path_value(), action).map_err(|_| {
                ServicePlanError::StaticPreflight {
                    service: service.name.clone(),
                    route: route_index,
                }
            })?,
        ),
    };
    Ok(HttpRouteBlueprint {
        route: compiled_route,
        access: route.access_policy.clone(),
        policy: RoutePolicyPlan::compile(route.policy),
        action,
    })
}

fn compile_cache_policy_blueprint(
    config: &oxiroute_config::ConfigDraft,
    service: &oxiroute_config::HttpService,
    route_index: usize,
    route: &oxiroute_config::HttpRoute,
    proxy: &HttpProxyPolicy,
) -> Result<Option<CachePolicyBlueprint>, ServicePlanError> {
    let Some(policy) = proxy.cache.as_deref() else {
        return Ok(None);
    };
    compile_cache_policy_values(
        config,
        &service.name,
        route_index,
        route.access_policy.is_some(),
        service.gzip.is_some(),
        &proxy.request_headers,
        policy,
    )
    .map(Some)
}

#[expect(
    clippy::too_many_lines,
    reason = "cache policy compilation validates one cohesive immutable decision"
)]
fn compile_cache_policy_values(
    config: &oxiroute_config::ConfigDraft,
    service: &str,
    route_index: usize,
    has_access_policy: bool,
    has_gzip: bool,
    request_headers: &[oxiroute_config::HttpRequestHeaderMutation],
    policy: &oxiroute_config::HttpCachePolicy,
) -> Result<CachePolicyBlueprint, ServicePlanError> {
    let unavailable = |policy| ServicePlanError::RuntimePolicyUnavailable { policy };
    if has_access_policy {
        return Err(unavailable(
            "http_services[].routes[].access_policy_with_cache",
        ));
    }
    if has_gzip {
        return Err(unavailable("http_services[].gzip_with_cache"));
    }
    if !request_headers.is_empty() {
        return Err(unavailable(
            "http_services[].routes[].action.policy.request_headers_with_cache",
        ));
    }
    if policy.key_components.as_slice()
        != [
            CacheKeyComponent::Scheme,
            CacheKeyComponent::NormalizedHost,
            CacheKeyComponent::PathAndQuery,
        ]
    {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.key_components",
        ));
    }
    if !policy.bypass_request.is_empty() {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.bypass_request",
        ));
    }
    if !policy.no_store_request.is_empty() {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.no_store_request",
        ));
    }
    if !policy.no_store_response.is_empty() {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.no_store_response",
        ));
    }
    if policy.set_cookie_policy != CacheSetCookiePolicy::Bypass {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.set_cookie_policy",
        ));
    }
    if policy.authorization_policy != CacheAuthorizationPolicy::Bypass {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.authorization_policy",
        ));
    }
    if policy.vary_policy != CacheVaryPolicy::Respect {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.vary_policy",
        ));
    }
    if !policy.stale_on.is_empty() {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.stale_on",
        ));
    }
    if !policy.collapsed_forwarding {
        return Err(unavailable(
            "http_services[].routes[].action.policy.cache.collapsed_forwarding",
        ));
    }
    let store = config
        .cache_stores
        .iter()
        .position(|store| store.common().name == policy.store)
        .ok_or_else(|| ServicePlanError::CacheRuntimeUnavailable {
            service: service.to_owned(),
            route: route_index,
        })?;
    let timeline = CacheTimeline::new(
        policy.use_origin_cache_control,
        Duration::from_millis(policy.default_ttl_ms),
        policy.status_ttls.iter().map(|entry| {
            (
                StatusCode::from_u16(entry.status).expect("validated cache status"),
                Duration::from_millis(entry.ttl_ms),
            )
        }),
        Duration::from_millis(policy.grace_ms),
        Duration::from_millis(policy.keep_ms),
    )
    .map_err(|_| unavailable("http_services[].routes[].action.policy.cache.timeline"))?;
    let methods = policy
        .methods
        .iter()
        .map(|method| method.parse::<Method>().expect("validated cache method"))
        .collect();
    let surrogate_header = policy.surrogate_tags.as_ref().map(|tags| {
        HeaderName::from_bytes(tags.response_header.as_bytes()).expect("validated surrogate header")
    });
    let surrogate_limits = policy
        .surrogate_tags
        .as_ref()
        .map(|tags| {
            Ok((
                usize::try_from(tags.max_tags).map_err(|_| unavailable("cache tag bounds"))?,
                usize::try_from(tags.max_tag_bytes).map_err(|_| unavailable("cache tag bounds"))?,
            ))
        })
        .transpose()?;
    Ok(CachePolicyBlueprint {
        store,
        timeline,
        methods,
        revalidate: policy.revalidate,
        surrogate_header,
        surrogate_limits,
        purge_authorization: policy.purge_authorization.clone(),
    })
}

fn compile_l4_blueprints(
    config: &oxiroute_config::ConfigDraft,
) -> Result<Box<[L4ServiceBlueprint]>, ServicePlanError> {
    config
        .l4_services
        .iter()
        .map(|service| {
            let pool = config
                .upstream_pools
                .iter()
                .position(|pool| pool.name == service.upstream_pool)
                .ok_or_else(|| ServicePlanError::UnknownL4Pool {
                    service: service.name.clone(),
                    pool: service.upstream_pool.clone(),
                })?;
            if config.upstream_pools[pool].tls.is_some() {
                return Err(ServicePlanError::TlsUpstreamPoolForL4Service {
                    service: service.name.clone(),
                    pool: service.upstream_pool.clone(),
                });
            }
            Ok(L4ServiceBlueprint {
                pool,
                connect_timeout: Duration::from_millis(service.connect_timeout_ms),
                idle_timeout: Duration::from_millis(service.idle_timeout_ms),
                lifetime_timeout: service.lifetime_timeout_ms.map(Duration::from_millis),
                proxy_protocol: service.proxy_protocol,
                udp: service.udp.unwrap_or_default(),
            })
        })
        .collect()
}

fn compile_listener_blueprints(
    config: &oxiroute_config::ConfigDraft,
) -> Result<Box<[ListenerBlueprint]>, ServicePlanError> {
    config
        .listeners
        .iter()
        .map(|listener| {
            let service_name = listener.service.as_deref().ok_or_else(|| {
                ServicePlanError::MissingListenerService {
                    listener: listener.name.clone(),
                }
            })?;
            let service = match listener.protocol {
                Protocol::Http | Protocol::Http3 => ServiceReference::Http(
                    config
                        .http_services
                        .iter()
                        .position(|service| service.name == service_name)
                        .ok_or_else(|| ServicePlanError::UnknownHttpService {
                            listener: listener.name.clone(),
                            service: service_name.into(),
                        })?,
                ),
                Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3 => {
                    ServiceReference::Forward(
                        config
                            .forward_proxy_services
                            .iter()
                            .position(|service| service.name == service_name)
                            .ok_or_else(|| ServicePlanError::UnknownForwardProxyService {
                                listener: listener.name.clone(),
                                service: service_name.into(),
                            })?,
                    )
                }
                Protocol::Rtmp => ServiceReference::Rtmp(
                    config
                        .rtmp_services
                        .iter()
                        .position(|service| service.name == service_name)
                        .ok_or_else(|| ServicePlanError::UnknownRtmpService {
                            listener: listener.name.clone(),
                            service: service_name.into(),
                        })?,
                ),
                Protocol::Tcp | Protocol::Udp => ServiceReference::L4(
                    config
                        .l4_services
                        .iter()
                        .position(|service| service.name == service_name)
                        .ok_or_else(|| {
                            if listener.protocol == Protocol::Udp {
                                ServicePlanError::UnknownUdpService {
                                    listener: listener.name.clone(),
                                    service: service_name.into(),
                                }
                            } else {
                                ServicePlanError::UnknownL4Service {
                                    listener: listener.name.clone(),
                                    service: service_name.into(),
                                }
                            }
                        })?,
                ),
            };
            let tls_profile = listener
                .tls_profile
                .as_deref()
                .map(|name| {
                    config
                        .tls_profiles
                        .iter()
                        .position(|profile| profile.name == name)
                        .ok_or_else(|| ServicePlanError::UnknownListenerTlsProfile {
                            listener: listener.name.clone(),
                            profile: name.into(),
                        })
                })
                .transpose()?;
            if matches!(
                listener.protocol,
                Protocol::Tcp | Protocol::Udp | Protocol::Rtmp
            ) && tls_profile.is_some()
            {
                return Err(ServicePlanError::UnexpectedListenerTlsProfile {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    profile: listener.tls_profile.clone().expect("present"),
                });
            }
            Ok(ListenerBlueprint {
                name: listener.name.clone(),
                bind: listener.bind.clone(),
                protocol: listener.protocol,
                service,
                tls_profile,
                proxy_protocol: listener.proxy_protocol,
                max_connections: listener.max_connections,
                downstream_timeouts: listener.downstream_timeouts,
            })
        })
        .collect()
}

fn compile_rtmp_blueprint(
    plan: RtmpServicePlan,
    service: &oxiroute_config::RtmpService,
) -> RtmpSpec {
    RtmpSpec {
        plan,
        access_log: service.access_log.clone(),
    }
}
