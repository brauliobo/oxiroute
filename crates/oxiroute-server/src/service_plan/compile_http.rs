#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn compile_http_services(
    services: &[HttpServiceBlueprint],
    cache_specs: &[CacheStoreBlueprint],
    pools: &[Arc<UpstreamPlan>],
    mode: AcquisitionMode,
    deadline: Option<Instant>,
) -> Result<Vec<Arc<HttpServicePlan>>, RuntimeAcquisitionError> {
    let mut http_services = Vec::with_capacity(services.len());
    let mut cache_backends = HashMap::new();
    for service in services {
        let mut route_plans = HashMap::with_capacity(service.routes.len());
        for (route_index, route) in service.routes.iter().enumerate() {
            let plan = compile_http_route(
                &service.name,
                route_index,
                route,
                pools,
                cache_specs,
                &mut cache_backends,
                service.gzip.is_some(),
                mode,
                deadline,
            )?;
            route_plans.insert(route_index.to_string(), plan);
        }
        http_services.push(Arc::new(HttpServicePlan::with_http_policy(
            service.automatic_response_headers,
            service.max_request_body_bytes,
            route_plans,
            service.upstream_io_timeout,
            service.route_table.clone(),
            service.gzip.clone().map(Arc::new),
            acquire_access_log(&service.name, service.access_log.as_ref(), false, mode)?,
        )));
    }
    Ok(http_services)
}

#[expect(
    clippy::too_many_arguments,
    reason = "route acquisition carries shared pools, caches, and the explicit acquisition mode"
)]
fn compile_http_route(
    service: &str,
    route_index: usize,
    route: &HttpRouteBlueprint,
    pools: &[Arc<UpstreamPlan>],
    cache_stores: &[CacheStoreBlueprint],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    _has_gzip: bool,
    mode: AcquisitionMode,
    deadline: Option<Instant>,
) -> Result<Arc<HttpRoutePlan>, RuntimeAcquisitionError> {
    let access = route
        .access
        .as_ref()
        .map(RouteAccess::load)
        .transpose()
        .map_err(|_| ServicePlanError::AccessPreflight {
            service: service.to_owned(),
            route: route_index,
        })?;
    let action = match &route.action {
        HttpActionBlueprint::Proxy {
            pool,
            rescue_pool,
            select,
            policy,
            cache,
        } => compile_proxy_action(
            service,
            route_index,
            *pool,
            *rescue_pool,
            select.as_ref(),
            policy,
            cache.as_ref(),
            pools,
            cache_stores,
            cache_backends,
            mode,
            deadline,
        )?,
        HttpActionBlueprint::Fixed(plan) => HttpActionPlan::Fixed(plan.clone()),
        HttpActionBlueprint::Redirect(plan) => HttpActionPlan::Redirect(plan.clone()),
        HttpActionBlueprint::Static(action) => {
            HttpActionPlan::Static(StaticFilesPlan::acquire(action).map_err(|_| {
                ServicePlanError::StaticPreflight {
                    service: service.to_owned(),
                    route: route_index,
                }
            })?)
        }
    };
    let route_id = Arc::from(route.route.route_id());
    let plan = Arc::new(HttpRoutePlan {
        access,
        action,
        policy: route.policy,
        route_id,
    });
    Ok(plan)
}

#[expect(
    clippy::too_many_arguments,
    reason = "proxy compilation carries route errors and shared cache-generation state"
)]
fn compile_proxy_action(
    service: &str,
    route: usize,
    pool_index: usize,
    rescue_pool_index: Option<usize>,
    select: Option<&(HttpPoolSelect, Vec<usize>)>,
    policy: &ProxyPolicyPlan,
    cache: Option<&CachePolicyBlueprint>,
    pools: &[Arc<UpstreamPlan>],
    cache_stores: &[CacheStoreBlueprint],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    mode: AcquisitionMode,
    deadline: Option<Instant>,
) -> Result<HttpActionPlan, RuntimeAcquisitionError> {
    let pool = &pools[pool_index];
    let cache = acquire_cache_policy(
        service,
        route,
        cache,
        cache_stores,
        cache_backends,
        mode,
        deadline,
    )?;
    Ok(HttpActionPlan::Proxy(ProxyActionPlan {
        pool: Arc::clone(pool),
        rescue_pool: rescue_pool_index.map(|index| Arc::clone(&pools[index])),
        select: select.map(|(select, indices)| {
            let rule_pools = indices
                .iter()
                .map(|index| Arc::clone(&pools[*index]))
                .collect();
            PoolSelectPlan::compile(select, rule_pools)
        }),
        policy: Box::new(policy.clone_with_cache(cache)),
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn acquire_cache_policy(
    service: &str,
    route: usize,
    policy: Option<&CachePolicyBlueprint>,
    stores: &[CacheStoreBlueprint],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    mode: AcquisitionMode,
    deadline: Option<Instant>,
) -> Result<Option<Arc<HttpCachePlan>>, RuntimeAcquisitionError> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    let unavailable = |name| ServicePlanError::RuntimePolicyUnavailable { policy: name };
    let store_name = stores[policy.store].name();
    let cache = if let Some(cache) = cache_backends.get(store_name) {
        Arc::clone(cache)
    } else {
        let cache = match &stores[policy.store] {
            CacheStoreBlueprint::Memory { config, .. } => Arc::new(HttpCacheBackend::Memory(
                Arc::new(Cache::new(config.clone()).map_err(|_| {
                    unavailable("http_services[].routes[].action.policy.cache.memory")
                })?),
            )),
            CacheStoreBlueprint::Disk { root, config, .. } => {
                if mode == AcquisitionMode::Validate {
                    DiskCache::validate(root, config).map_err(|_| {
                        unavailable("http_services[].routes[].action.policy.cache.disk")
                    })?;
                    Arc::new(HttpCacheBackend::Memory(Arc::new(
                        Cache::new(config.memory.clone()).map_err(|_| {
                            unavailable("http_services[].routes[].action.policy.cache.memory")
                        })?,
                    )))
                } else {
                    let backend = open_shared_disk_backend_with_deadline(root, config, deadline)
                        .map_err(|error| match error {
                            DiskBackendOpenError::Unavailable => {
                                RuntimeAcquisitionError::ServicePlan(unavailable(
                                    "http_services[].routes[].action.policy.cache.disk",
                                ))
                            }
                            DiskBackendOpenError::TimedOut => {
                                RuntimeAcquisitionError::PreparationTimedOut
                            }
                        })?;
                    Arc::new(HttpCacheBackend::Disk(backend))
                }
            }
        };
        cache_backends.insert(store_name.to_owned(), Arc::clone(&cache));
        cache
    };
    let purge_access = policy
        .purge_authorization
        .as_ref()
        .map(|authorization| match authorization {
            CachePurgeAuthorization::BearerTokenFile { token_file_path } => {
                CachePurgeAccess::load(token_file_path).map_err(|_| {
                    ServicePlanError::AccessPreflight {
                        service: service.to_owned(),
                        route,
                    }
                })
            }
        })
        .transpose()?;
    Ok(Some(Arc::new(HttpCachePlan {
        cache,
        timeline: policy.timeline.clone(),
        methods: policy.methods.clone(),
        revalidate: policy.revalidate,
        surrogate_header: policy.surrogate_header.clone(),
        surrogate_limits: policy.surrogate_limits,
        purge_access,
    })))
}

pub(super) fn acquire_access_log(
    service: &str,
    policy: Option<&oxiroute_config::AccessLogPolicy>,
    rtmp: bool,
    mode: AcquisitionMode,
) -> Result<Option<Arc<AccessLog>>, ServicePlanError> {
    let invalid = |_| ServicePlanError::AccessLogPreflight {
        service: service.to_owned(),
    };
    if mode == AcquisitionMode::Validate {
        AccessLog::validate(policy).map_err(invalid)?;
        Ok(None)
    } else if rtmp {
        AccessLog::open_rtmp(service, policy)
            .map_err(invalid)
            .map(|log| log.map(Arc::new))
    } else {
        AccessLog::open(service, policy)
            .map_err(invalid)
            .map(|log| log.map(Arc::new))
    }
}
