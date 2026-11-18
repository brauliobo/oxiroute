#[allow(clippy::wildcard_imports)]
use super::*;
use crate::{
    http::proxy::lane::{LaneRoute, RouteEdits},
    label::Label,
};

pub(super) fn compile_http_services(
    services: &[HttpServiceBlueprint],
    cache_specs: &[CacheStoreBlueprint],
    pools: &[Arc<UpstreamPlan>],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    mode: AcquisitionMode,
) -> Result<Vec<Arc<HttpServicePlan>>, ServicePlanError> {
    let mut http_services = Vec::with_capacity(services.len());
    for service in services {
        let mut route_plans = Vec::with_capacity(service.routes.len());
        for (route_index, route) in service.routes.iter().enumerate() {
            route_plans.push(compile_http_route(
                &service.name,
                route_index,
                route,
                pools,
                cache_specs,
                cache_backends,
                service.gzip.is_some(),
            )?);
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

fn compile_http_route(
    service: &str,
    route_index: usize,
    route: &HttpRouteBlueprint,
    pools: &[Arc<UpstreamPlan>],
    cache_stores: &[CacheStoreBlueprint],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    _has_gzip: bool,
) -> Result<Arc<HttpRoutePlan>, ServicePlanError> {
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
    let route_id = Label::new(route.route.route_id());
    let lane = LaneRoute::compile(&action, &route.policy, access.as_ref());
    let plan = Arc::new(HttpRoutePlan {
        edits: RouteEdits::compile(lane, &action, &route_id),
        engine: crate::http::proxy::engine::plan(lane, &action, route.policy),
        lane,
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
) -> Result<HttpActionPlan, ServicePlanError> {
    let pool = &pools[pool_index];
    let cache = acquire_cache_policy(service, route, cache, cache_stores, cache_backends)?;
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

pub(super) fn acquire_cache_policy(
    service: &str,
    route: usize,
    policy: Option<&CachePolicyBlueprint>,
    stores: &[CacheStoreBlueprint],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
) -> Result<Option<Arc<HttpCachePlan>>, ServicePlanError> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    let unavailable = |name| ServicePlanError::RuntimePolicyUnavailable { policy: name };
    let store_name = stores[policy.store].name();
    let cache = if let Some(cache) = cache_backends.get(store_name) {
        Arc::clone(cache)
    } else {
        let cache = match &stores[policy.store] {
            CacheStoreBlueprint::Memory { config, .. } => {
                Arc::new(HttpCacheBackend::Memory(Arc::new(
                    crate::http::cache::memory_cache(config.clone()).map_err(|_| {
                        unavailable("http_services[].routes[].action.policy.cache.memory")
                    })?,
                )))
            }
            CacheStoreBlueprint::Disk { name, root, config } => {
                DiskCache::validate(root, config).map_err(|source| {
                    ServicePlanError::DiskCacheStore {
                        store: name.clone(),
                        root: root.clone(),
                        source,
                    }
                })?;
                Arc::new(HttpCacheBackend::Disk(Arc::new(DiskBackend::new(
                    name.clone(),
                    root.clone(),
                    config.clone(),
                ))))
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
    let compression = acquire_cache_compression(policy, &cache, unavailable)?;
    Ok(Some(Arc::new(HttpCachePlan {
        cache,
        timeline: policy.timeline.clone(),
        methods: policy.methods.clone(),
        revalidate: policy.revalidate,
        surrogate_header: policy.surrogate_header.clone(),
        surrogate_limits: policy.surrogate_limits,
        purge_access,
        compression,
    })))
}

fn acquire_cache_compression(
    policy: &CachePolicyBlueprint,
    backend: &HttpCacheBackend,
    unavailable: impl Fn(&'static str) -> ServicePlanError,
) -> Result<Option<CacheCompression>, ServicePlanError> {
    let Some(compression) = &policy.compression else {
        return Ok(None);
    };
    match backend {
        HttpCacheBackend::Memory(cache) => Ok(Some(CacheCompression::new(
            compression.clone(),
            Arc::clone(cache),
        ))),
        HttpCacheBackend::Disk(_) => Err(unavailable(
            "http_services[].routes[].action.policy.cache.compression_on_disk_store",
        )),
    }
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
