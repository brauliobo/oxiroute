use std::{
    collections::{HashMap, HashSet},
    net::ToSocketAddrs,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crate::{
    CertbotReconciler, HealthBuildError, HealthSupervisor, L4ServicePlan, PoolError, PreparedTls,
    RelayPolicy, RoundRobinPool, Route, RouteError, RouteTable, RuntimeEndpoint, TlsBuildError,
    TlsProfilePlan, TopologySnapshot, health,
    http_action::{
        AccessLog, FixedResponsePlan, HttpActionPlan, HttpGzipPlan, HttpRoutePlan, ProxyActionPlan,
        ProxyPolicyPlan, RedirectPlan, RouteAccess, RoutePolicyPlan, StaticFilesPlan,
    },
    routing::RuntimeServer,
    upstream_peer::UpstreamPlan,
};
use http::{Method, Uri, uri::Authority};
use oxiroute_config::{
    AccessLogPolicy, Config, DnsResolutionPolicy, HttpProxyPolicy, HttpRoute as ConfigHttpRoute,
    HttpRouteAction, ListenerBind, Protocol, RtmpRecorderStart as ConfigRecorderStart,
};
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, RecorderWorkerConfig, RecordingPathPolicy, RecordingSegmentNaming,
    RecordingStore, RecordingStoreLimits, RecordingTimeBasis, RecordingTimezone,
    RtmpApplication as RuntimeRtmpApplication, RtmpCapabilities, RtmpPushApplication,
    RtmpPushTarget, RtmpRecorderPolicy, RtmpRecorderStart, RtmpRegistry, RtmpRelayConfig,
    RtmpServiceRuntime, RtmpSessionPolicy,
};

#[derive(Clone, Debug)]
pub struct ServiceSpec {
    pub name: String,
    pub bind: ListenerBind,
    pub max_connections: Option<u64>,
    pub downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy,
    pub kind: ServiceKind,
    pub tls: Option<Arc<TlsProfilePlan>>,
}

#[derive(Clone, Debug)]
pub enum ServiceKind {
    Http(Arc<HttpServicePlan>),
    Rtmp(Arc<RtmpServicePlan>),
    Tcp(Arc<L4ServicePlan>),
}

impl ServiceKind {
    #[must_use]
    pub const fn protocol(&self) -> &'static str {
        match self {
            Self::Http(_) => "http",
            Self::Rtmp(_) => "rtmp",
            Self::Tcp(_) => "tcp",
        }
    }
}

pub struct RtmpServicePlan {
    service_id: String,
    outbound_chunk_size: u32,
    hub: LiveHub,
    applications: Vec<PreparedRtmpApplication>,
}

impl std::fmt::Debug for RtmpServicePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtmpServicePlan")
            .field("service_id", &self.service_id)
            .finish_non_exhaustive()
    }
}

impl RtmpServicePlan {
    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Opens this service's preflighted recording stores and creates its process runtime.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when a recording root cannot be opened and pinned at startup.
    pub fn runtime(
        &self,
        registry: Arc<RtmpRegistry>,
    ) -> Result<RtmpServiceRuntime, ServicePlanError> {
        let mut stores = HashMap::<PathBuf, RecordingStore>::new();
        let applications =
            self.applications
                .iter()
                .map(|application| {
                    let recorders = application
                        .recorders
                        .iter()
                        .map(|recorder| {
                            let store =
                                if let Some(store) = stores.get(&recorder.root_directory) {
                                    store.clone()
                                } else {
                                    let store = RecordingStore::open(
                                        &recorder.root_directory,
                                        recorder.store_limits,
                                    )
                                    .map_err(|_| ServicePlanError::RecorderStartup {
                                        service: self.service_id.clone(),
                                        application: application.name.clone(),
                                        recorder: recorder.name.clone(),
                                    })?;
                                    stores.insert(recorder.root_directory.clone(), store.clone());
                                    store
                                };
                            Ok(RtmpRecorderPolicy::new(
                                &recorder.name,
                                recorder.start,
                                store,
                                recorder.path_policy.clone(),
                                recorder.worker_config,
                            ))
                        })
                        .collect::<Result<Vec<_>, ServicePlanError>>()?;
                    Ok(RuntimeRtmpApplication::with_runtime(
                        &application.name,
                        application.live,
                        application.idle_streams,
                        application.hub.clone(),
                        application.push_targets.clone(),
                        recorders,
                    ))
                })
                .collect::<Result<Vec<_>, ServicePlanError>>()?;
        Ok(RtmpServiceRuntime::new(
            self.service_id.clone(),
            registry,
            self.hub.clone(),
            RtmpSessionPolicy::with_outbound_chunk_size(applications, self.outbound_chunk_size),
        ))
    }

    #[must_use]
    pub fn hub(&self) -> LiveHub {
        self.hub.clone()
    }

    #[must_use]
    pub fn manual_recording(&self) -> bool {
        self.applications.iter().any(|application| {
            application
                .recorders
                .iter()
                .any(|recorder| recorder.start == RtmpRecorderStart::Manual)
        })
    }

    #[must_use]
    pub fn recording_supported(&self) -> bool {
        self.applications
            .iter()
            .any(|application| !application.recorders.is_empty())
    }
}

#[derive(Clone)]
struct PreparedRtmpApplication {
    name: String,
    live: bool,
    idle_streams: bool,
    hub: LiveHub,
    push_targets: Vec<RtmpPushTarget>,
    recorders: Vec<PreparedRtmpRecorder>,
}

#[derive(Clone)]
struct PreparedRtmpRecorder {
    name: String,
    start: RtmpRecorderStart,
    root_directory: PathBuf,
    path_policy: RecordingPathPolicy,
    worker_config: RecorderWorkerConfig,
    store_limits: RecordingStoreLimits,
}

#[derive(Debug)]
pub struct HttpServicePlan {
    access_log: Option<Arc<AccessLog>>,
    gzip: Option<Arc<HttpGzipPlan>>,
    max_request_body_bytes: Option<u64>,
    route_plans: HashMap<String, Arc<HttpRoutePlan>>,
    upstream_io_timeout: Duration,
    routes: RouteTable,
}

impl HttpServicePlan {
    #[cfg(test)]
    pub(crate) const fn new(
        max_request_body_bytes: Option<u64>,
        route_plans: HashMap<String, Arc<HttpRoutePlan>>,
        upstream_io_timeout: Duration,
        routes: RouteTable,
    ) -> Self {
        Self {
            access_log: None,
            gzip: None,
            max_request_body_bytes,
            route_plans,
            upstream_io_timeout,
            routes,
        }
    }

    fn with_http_policy(
        max_request_body_bytes: Option<u64>,
        route_plans: HashMap<String, Arc<HttpRoutePlan>>,
        upstream_io_timeout: Duration,
        routes: RouteTable,
        gzip: Option<Arc<HttpGzipPlan>>,
        access_log: Option<Arc<AccessLog>>,
    ) -> Self {
        Self {
            access_log,
            gzip,
            max_request_body_bytes,
            route_plans,
            upstream_io_timeout,
            routes,
        }
    }

    pub(crate) fn select_route(
        &self,
        authority: Option<&Authority>,
        uri: &Uri,
        method: &Method,
    ) -> Option<Arc<HttpRoutePlan>> {
        let route = self.routes.select(authority, uri, method)?;
        self.route_plans.get(route.route_id()).cloned()
    }

    #[must_use]
    pub fn select(
        &self,
        authority: Option<&Authority>,
        uri: &Uri,
        method: &Method,
    ) -> Option<crate::EndpointLease> {
        self.select_route(authority, uri, method)
            .and_then(|route| match &route.action {
                HttpActionPlan::Proxy(proxy) => proxy.pool.selector().select(),
                HttpActionPlan::Fixed(_)
                | HttpActionPlan::Redirect(_)
                | HttpActionPlan::Static(_) => None,
            })
    }

    #[must_use]
    pub const fn upstream_io_timeout(&self) -> Duration {
        self.upstream_io_timeout
    }

    #[must_use]
    pub const fn max_request_body_bytes(&self) -> Option<u64> {
        self.max_request_body_bytes
    }

    pub(crate) fn gzip(&self) -> Option<&Arc<HttpGzipPlan>> {
        self.gzip.as_ref()
    }

    pub(crate) fn access_log(&self) -> Option<&Arc<AccessLog>> {
        self.access_log.as_ref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServicePlanError {
    #[error("runtime configuration is invalid: {0}")]
    InvalidConfig(#[source] Box<oxiroute_config::ConfigError>),
    #[error("TLS configuration cannot be prepared: {0}")]
    Tls(#[source] Box<TlsBuildError>),
    #[error("upstream pool `{pool}` cannot be compiled: {source}")]
    Pool { pool: String, source: PoolError },
    #[error("upstream pool `{pool}` health check cannot be compiled: {source}")]
    Health {
        pool: String,
        source: Box<HealthBuildError>,
    },
    #[error("health-enabled configurations require `runtime_plan` so probes remain active")]
    HealthSupervisorRequired,
    #[error("HTTP service `{service}` route {route} has invalid method `{method}`")]
    InvalidMethod {
        service: String,
        route: usize,
        method: String,
    },
    #[error("HTTP service `{service}` route {route} cannot be compiled: {source}")]
    Route {
        service: String,
        route: usize,
        source: RouteError,
    },
    #[error("HTTP service `{service}` route {route} access policy failed secure preflight")]
    AccessPreflight { service: String, route: usize },
    #[error("HTTP service `{service}` route {route} static root failed secure preflight")]
    StaticPreflight { service: String, route: usize },
    #[error("HTTP service `{service}` access log failed secure preflight")]
    AccessLogPreflight { service: String },
    #[error("HTTP service `{service}` route {route} references unknown pool `{pool}`")]
    UnknownHttpPool {
        service: String,
        route: usize,
        pool: String,
    },
    #[error(
        "HTTP service `{service}` route {route} configures cache, but cache runtime is unavailable"
    )]
    CacheRuntimeUnavailable { service: String, route: usize },
    #[error("listener `{listener}` requires a configured service")]
    MissingListenerService { listener: String },
    #[error("HTTP listener `{listener}` references unknown service `{service}`")]
    UnknownHttpService { listener: String, service: String },
    #[error("TCP listener `{listener}` references unknown service `{service}`")]
    UnknownL4Service { listener: String, service: String },
    #[error("RTMP listener `{listener}` references unknown service `{service}`")]
    UnknownRtmpService { listener: String, service: String },
    #[error("forward proxy runtime is not integrated for listener `{listener}`")]
    ForwardProxyRuntimeUnavailable { listener: String },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` has an invalid runtime policy"
    )]
    InvalidRecorderPolicy {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` failed recording-root preflight"
    )]
    RecorderPreflight {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` could not start"
    )]
    RecorderStartup {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP push target {target} in application `{application}` of service `{service}` cannot be resolved safely"
    )]
    RtmpPushResolution {
        service: String,
        application: String,
        target: usize,
    },
    #[error(
        "RTMP push target {target} in application `{application}` of service `{service}` resolves to an active RTMP listener"
    )]
    RtmpPushDirectLoop {
        service: String,
        application: String,
        target: usize,
    },
    #[error("listener `{listener}` references unknown TLS profile `{profile}`")]
    UnknownListenerTlsProfile { listener: String, profile: String },
    #[error("{protocol:?} listener `{listener}` must not use TLS profile `{profile}`")]
    UnexpectedListenerTlsProfile {
        listener: String,
        protocol: Protocol,
        profile: String,
    },
    #[error("L4 service `{service}` references unknown pool `{pool}`")]
    UnknownL4Pool { service: String, pool: String },
    #[error("L4 service `{service}` references TLS-enabled upstream pool `{pool}`")]
    TlsUpstreamPoolForL4Service { service: String, pool: String },
    #[error("runtime does not yet implement canonical policy `{policy}`")]
    RuntimePolicyUnavailable { policy: &'static str },
}

/// Compiles validated listener definitions into runtime service specifications.
///
/// # Errors
///
/// Returns an error when a programmatically constructed configuration contains invalid routes,
/// pools, service references, or listener/service protocol relationships.
pub fn service_specs(config: &Config) -> Result<Vec<ServiceSpec>, ServicePlanError> {
    if config
        .upstream_pools
        .iter()
        .any(|pool| pool.health_check.is_some())
    {
        return Err(ServicePlanError::HealthSupervisorRequired);
    }
    Ok(runtime_plan(config)?.services)
}

pub struct RuntimePlan {
    pub max_connections: Option<u64>,
    pub services: Vec<ServiceSpec>,
    pub health_supervisor: Option<HealthSupervisor>,
    pub pools: Vec<Arc<RoundRobinPool>>,
    pub rtmp_capabilities: RtmpCapabilities,
    pub rtmp_recording_supported: bool,
    pub tls: PreparedTls,
    pub topology: Arc<TopologySnapshot>,
}

impl RuntimePlan {
    #[must_use]
    pub fn certbot_reconcilers(&self) -> &[Arc<CertbotReconciler>] {
        self.tls.certbot_reconcilers()
    }
}

/// Compiles one immutable runtime generation including traffic and health services.
///
/// # Errors
///
/// Returns an error when a pool, route, reference, or health probe cannot be compiled.
pub fn runtime_plan(config: &Config) -> Result<RuntimePlan, ServicePlanError> {
    let mut config = config.clone();
    oxiroute_config::validate_config(&mut config)
        .map_err(|source| ServicePlanError::InvalidConfig(Box::new(source)))?;
    reject_unimplemented_runtime_policies(&config)?;
    let tls = crate::tls::prepare_tls(&config)
        .map_err(|source| ServicePlanError::Tls(Box::new(source)))?;
    let pools = compile_pools(&config)?;
    let http_services = compile_http_services(&config, &pools.by_name)?;
    let rtmp_services = compile_rtmp_services(&config)?;
    let l4_services = compile_l4_services(&config, &pools.by_name)?;

    let services = config
        .listeners
        .iter()
        .map(|listener| {
            compile_listener(
                listener,
                &http_services,
                &rtmp_services,
                &l4_services,
                tls.profiles(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let topology = Arc::new(TopologySnapshot::compile(
        &config,
        &services,
        &pools.ordered,
    ));
    let health_supervisor =
        (!pools.health_groups.is_empty()).then(|| HealthSupervisor::new(pools.health_groups));
    let mut active_rtmp_services = services.iter().filter_map(|service| match &service.kind {
        ServiceKind::Rtmp(service) => Some(service.as_ref()),
        ServiceKind::Http(_) | ServiceKind::Tcp(_) => None,
    });
    let rtmp_capabilities = RtmpCapabilities {
        live_ingest: active_rtmp_services.clone().next().is_some(),
        manual_recording: active_rtmp_services
            .clone()
            .any(RtmpServicePlan::manual_recording),
    };
    let rtmp_recording_supported = active_rtmp_services.any(RtmpServicePlan::recording_supported);
    Ok(RuntimePlan {
        max_connections: config.max_connections,
        services,
        health_supervisor,
        pools: pools.ordered,
        rtmp_capabilities,
        rtmp_recording_supported,
        tls,
        topology,
    })
}

struct CompiledPools {
    by_name: Arc<HashMap<String, Arc<UpstreamPlan>>>,
    health_groups: Vec<health::HealthGroup>,
    ordered: Vec<Arc<RoundRobinPool>>,
}

#[expect(
    clippy::too_many_lines,
    reason = "pool compilation performs one atomic validation and construction pass"
)]
fn compile_pools(config: &Config) -> Result<CompiledPools, ServicePlanError> {
    let protected_addresses: Arc<[std::net::SocketAddr]> = config
        .management
        .iter()
        .map(|management| management.bind)
        .chain(
            config
                .stats
                .iter()
                .flat_map(|stats| stats.binds.iter().copied()),
        )
        .collect();
    let mut pools = HashMap::with_capacity(config.upstream_pools.len());
    let mut health_groups = Vec::new();
    let mut ordered = Vec::with_capacity(config.upstream_pools.len());
    for pool in &config.upstream_pools {
        let servers = pool
            .servers
            .iter()
            .map(|server| {
                let endpoint = RuntimeEndpoint::try_from(&server.endpoint)?;
                let pinned_addresses: Option<Arc<[std::net::SocketAddr]>> = if server.dns_resolution
                    == DnsResolutionPolicy::Startup
                {
                    let oxiroute_config::UpstreamEndpoint::Dns { host, port } = &server.endpoint
                    else {
                        unreachable!(
                            "configuration validation restricts startup DNS to DNS servers"
                        );
                    };
                    let addresses = (host.as_str(), *port).to_socket_addrs().map_err(|error| {
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
                    (oxiroute_config::UpstreamEndpoint::Socket { address }, _) => {
                        std::slice::from_ref(address)
                    }
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
                    protected_addresses: Arc::clone(&protected_addresses),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            })?;
        let selector = Arc::new(
            RoundRobinPool::new_named_servers(
                pool.name.clone(),
                servers,
                pool.algorithm,
                pool.health_check.as_ref().map(|health| health.startup),
                pool.queue_timeout_ms.map(Duration::from_millis),
            )
            .map_err(|source| ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            })?,
        );
        let tls = crate::tls::prepare_upstream_tls(pool)
            .map_err(|source| ServicePlanError::Tls(Box::new(source)))?
            .map(Arc::new);
        let compiled = Arc::new(UpstreamPlan::with_policy(
            Arc::clone(&selector),
            tls,
            pool.connect_timeout_ms.map(Duration::from_millis),
            pool.server_timeout_ms.map(Duration::from_millis),
            pool.connection_reuse,
        ));
        if let Some(health_check) = &pool.health_check {
            health_groups.push(
                health::compile_health_group(&pool.name, &selector, health_check).map_err(
                    |source| ServicePlanError::Health {
                        pool: pool.name.clone(),
                        source: Box::new(source),
                    },
                )?,
            );
        }
        pools.insert(pool.name.clone(), Arc::clone(&compiled));
        ordered.push(selector);
    }
    Ok(CompiledPools {
        by_name: Arc::new(pools),
        health_groups,
        ordered,
    })
}

fn compile_http_services(
    config: &Config,
    pools: &Arc<HashMap<String, Arc<UpstreamPlan>>>,
) -> Result<HashMap<String, Arc<HttpServicePlan>>, ServicePlanError> {
    let mut http_services = HashMap::with_capacity(config.http_services.len());
    for service in &config.http_services {
        let mut routes = Vec::with_capacity(service.routes.len());
        let mut route_plans = HashMap::with_capacity(service.routes.len());
        for (route_index, route) in service.routes.iter().enumerate() {
            let (compiled_route, plan) =
                compile_http_route(&service.name, route_index, route, pools)?;
            routes.push(compiled_route);
            route_plans.insert(route_index.to_string(), plan);
        }
        http_services.insert(
            service.name.clone(),
            Arc::new(HttpServicePlan::with_http_policy(
                service.max_request_body_bytes,
                route_plans,
                Duration::from_millis(service.upstream_io_timeout_ms),
                RouteTable::new(routes),
                service
                    .gzip
                    .as_ref()
                    .map(HttpGzipPlan::compile)
                    .map(Arc::new),
                AccessLog::open(&service.name, service.access_log.as_ref())
                    .map_err(|_| ServicePlanError::AccessLogPreflight {
                        service: service.name.clone(),
                    })?
                    .map(Arc::new),
            )),
        );
    }
    Ok(http_services)
}

fn compile_http_route(
    service: &str,
    route_index: usize,
    route: &ConfigHttpRoute,
    pools: &Arc<HashMap<String, Arc<UpstreamPlan>>>,
) -> Result<(Route, Arc<HttpRoutePlan>), ServicePlanError> {
    let methods = if route.methods.is_empty() {
        None
    } else {
        Some(
            route
                .methods
                .iter()
                .map(|method| {
                    method
                        .parse::<Method>()
                        .map_err(|_| ServicePlanError::InvalidMethod {
                            service: service.to_owned(),
                            route: route_index,
                            method: method.clone(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    };
    let access = route
        .access_policy
        .as_ref()
        .map(RouteAccess::load)
        .transpose()
        .map_err(|_| ServicePlanError::AccessPreflight {
            service: service.to_owned(),
            route: route_index,
        })?;
    let action = match &route.action {
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } => compile_proxy_action(service, route_index, upstream_pool, policy, pools)?,
        HttpRouteAction::FixedResponse {
            status,
            body,
            headers,
        } => HttpActionPlan::Fixed(FixedResponsePlan::compile(*status, body, headers)),
        HttpRouteAction::Redirect {
            status,
            location,
            headers,
        } => HttpActionPlan::Redirect(RedirectPlan {
            status: *status,
            location: location.clone(),
            headers: headers
                .iter()
                .filter(|header| {
                    header.always || crate::http_action::nginx_add_header_status(*status)
                })
                .map(|header| {
                    (
                        http::HeaderName::from_bytes(header.name.as_bytes())
                            .expect("validated redirect header name"),
                        header
                            .value
                            .parse::<http::HeaderValue>()
                            .expect("validated redirect header value"),
                    )
                })
                .collect(),
        }),
        action @ HttpRouteAction::StaticFiles { .. } => HttpActionPlan::Static(
            StaticFilesPlan::open(http_path_value(&route.path), action).map_err(|_| {
                ServicePlanError::StaticPreflight {
                    service: service.to_owned(),
                    route: route_index,
                }
            })?,
        ),
    };
    let route_id = route_index.to_string();
    let compiled_route = Route::new(
        route.host.clone(),
        route.path.clone(),
        methods,
        route_id.clone(),
    )
    .map_err(|source| ServicePlanError::Route {
        service: service.to_owned(),
        route: route_index,
        source,
    })?;
    let plan = Arc::new(HttpRoutePlan {
        access,
        action,
        policy: RoutePolicyPlan::compile(route.policy),
        route_id,
    });
    Ok((compiled_route, plan))
}

fn http_path_value(path: &oxiroute_config::HttpPathSelector) -> &str {
    match path {
        oxiroute_config::HttpPathSelector::SegmentPrefix { value }
        | oxiroute_config::HttpPathSelector::RawPrefix { value }
        | oxiroute_config::HttpPathSelector::Exact { value } => value,
    }
}

fn reject_unimplemented_runtime_policies(config: &Config) -> Result<(), ServicePlanError> {
    let unavailable = |policy| ServicePlanError::RuntimePolicyUnavailable { policy };
    for service in &config.http_services {
        for route in &service.routes {
            if route.policy.request_buffering || route.policy.response_buffering {
                return Err(unavailable("http_services[].routes[].policy.buffering_on"));
            }
        }
    }
    for service in &config.rtmp_services {
        if matches!(service.access_log, Some(AccessLogPolicy::File { .. })) {
            return Err(unavailable("rtmp_services[].access_log.file"));
        }
    }
    Ok(())
}

fn compile_proxy_action(
    service: &str,
    route: usize,
    upstream_pool: &str,
    policy: &HttpProxyPolicy,
    pools: &HashMap<String, Arc<UpstreamPlan>>,
) -> Result<HttpActionPlan, ServicePlanError> {
    if policy.cache.is_some() {
        return Err(ServicePlanError::CacheRuntimeUnavailable {
            service: service.into(),
            route,
        });
    }
    let pool = pools
        .get(upstream_pool)
        .ok_or_else(|| ServicePlanError::UnknownHttpPool {
            service: service.into(),
            route,
            pool: upstream_pool.into(),
        })?;
    Ok(HttpActionPlan::Proxy(ProxyActionPlan {
        pool: Arc::clone(pool),
        policy: ProxyPolicyPlan::compile(policy),
    }))
}

fn compile_l4_services(
    config: &Config,
    pools: &Arc<HashMap<String, Arc<UpstreamPlan>>>,
) -> Result<HashMap<String, Arc<L4ServicePlan>>, ServicePlanError> {
    let mut l4_services = HashMap::with_capacity(config.l4_services.len());
    for service in &config.l4_services {
        let Some(pool) = pools.get(&service.upstream_pool) else {
            return Err(ServicePlanError::UnknownL4Pool {
                service: service.name.clone(),
                pool: service.upstream_pool.clone(),
            });
        };
        if pool.tls().is_some() {
            return Err(ServicePlanError::TlsUpstreamPoolForL4Service {
                service: service.name.clone(),
                pool: service.upstream_pool.clone(),
            });
        }
        l4_services.insert(
            service.name.clone(),
            Arc::new(L4ServicePlan::new(
                RelayPolicy {
                    connect: pool
                        .connect_timeout(Duration::from_millis(service.connect_timeout_ms)),
                    idle: Some(Duration::from_millis(service.idle_timeout_ms)),
                    lifetime: service.lifetime_timeout_ms.map(Duration::from_millis),
                },
                Arc::clone(pool.selector()),
            )),
        );
    }
    Ok(l4_services)
}

fn compile_rtmp_services(
    config: &Config,
) -> Result<HashMap<String, Arc<RtmpServicePlan>>, ServicePlanError> {
    let listener_addresses: Vec<_> = config
        .listeners
        .iter()
        .filter(|listener| listener.protocol == Protocol::Rtmp)
        .filter_map(|listener| match listener.bind {
            ListenerBind::Socket { address } => Some(address),
            ListenerBind::Udp { .. } | ListenerBind::Unix { .. } => None,
        })
        .collect();
    let mut preflighted_roots = HashSet::new();
    let mut services = HashMap::with_capacity(config.rtmp_services.len());
    for service in &config.rtmp_services {
        let mut prepared_applications = Vec::with_capacity(service.applications.len());
        for application in &service.applications {
            let (hub, max_queue_messages, max_queue_bytes) = compile_rtmp_fanout(application)?;
            let push_targets = compile_rtmp_push_targets(
                &service.name,
                application,
                &listener_addresses,
                max_queue_messages,
                max_queue_bytes,
            )?;
            let mut prepared_recorders = Vec::with_capacity(application.recorders.len());
            for recorder in &application.recorders {
                prepared_recorders.push(compile_rtmp_recorder(
                    &service.name,
                    &application.name,
                    recorder,
                    &mut preflighted_roots,
                )?);
            }
            prepared_applications.push(PreparedRtmpApplication {
                name: application.name.clone(),
                live: application.live,
                idle_streams: application.idle_streams,
                hub,
                push_targets,
                recorders: prepared_recorders,
            });
        }
        let service_hub = prepared_applications.first().map_or_else(
            || LiveHub::new(LiveHubLimits::default()),
            |application| application.hub.clone(),
        );
        services.insert(
            service.name.clone(),
            Arc::new(RtmpServicePlan {
                service_id: service.name.clone(),
                outbound_chunk_size: service.outbound_chunk_size,
                hub: service_hub,
                applications: prepared_applications,
            }),
        );
    }
    Ok(services)
}

fn compile_rtmp_fanout(
    application: &oxiroute_config::RtmpApplication,
) -> Result<(LiveHub, usize, usize), ServicePlanError> {
    let unavailable = |policy| ServicePlanError::RuntimePolicyUnavailable { policy };
    let max_subscribers = usize::try_from(application.fanout.max_subscribers)
        .map_err(|_| unavailable("rtmp_services[].applications[].fanout.max_subscribers"))?;
    let max_queue_messages = usize::try_from(application.fanout.max_queue_messages_per_subscriber)
        .map_err(|_| {
            unavailable("rtmp_services[].applications[].fanout.max_queue_messages_per_subscriber")
        })?;
    let max_queue_bytes = usize::try_from(application.fanout.max_queue_bytes_per_subscriber)
        .map_err(|_| {
            unavailable("rtmp_services[].applications[].fanout.max_queue_bytes_per_subscriber")
        })?;
    let max_fanout_bytes = max_subscribers
        .checked_mul(max_queue_bytes)
        .ok_or_else(|| unavailable("rtmp_services[].applications[].fanout"))?;
    let hub = LiveHub::new(LiveHubLimits {
        max_subscribers,
        max_subscribers_per_stream: max_subscribers,
        max_queue_messages_per_subscriber: max_queue_messages,
        max_queue_bytes_per_subscriber: max_queue_bytes,
        max_fanout_bytes,
        ..LiveHubLimits::default()
    });
    Ok((hub, max_queue_messages, max_queue_bytes))
}

fn compile_rtmp_push_targets(
    service: &str,
    application: &oxiroute_config::RtmpApplication,
    listener_addresses: &[std::net::SocketAddr],
    max_queue_messages: usize,
    max_queue_bytes: usize,
) -> Result<Vec<RtmpPushTarget>, ServicePlanError> {
    application
        .push_targets
        .iter()
        .enumerate()
        .map(|(target_index, target)| {
            let resolution = || ServicePlanError::RtmpPushResolution {
                service: service.to_owned(),
                application: application.name.clone(),
                target: target_index,
            };
            let mut addresses: Vec<_> = (target.host.as_str(), target.port)
                .to_socket_addrs()
                .map_err(|_| resolution())?
                .take(33)
                .collect();
            if addresses.is_empty() || addresses.len() > 32 {
                return Err(resolution());
            }
            addresses.sort_unstable();
            addresses.dedup();
            if addresses.iter().any(|destination| {
                listener_addresses
                    .iter()
                    .any(|listener| socket_listener_contains(*listener, *destination))
            }) {
                return Err(ServicePlanError::RtmpPushDirectLoop {
                    service: service.to_owned(),
                    application: application.name.clone(),
                    target: target_index,
                });
            }
            Ok(RtmpPushTarget {
                address: addresses[0],
                application: if target.application == "$name" {
                    RtmpPushApplication::StreamName
                } else {
                    RtmpPushApplication::Exact(target.application.clone())
                },
                config: RtmpRelayConfig {
                    max_queue_messages,
                    max_queue_bytes,
                    ..RtmpRelayConfig::default()
                },
            })
        })
        .collect()
}

fn compile_rtmp_recorder(
    service: &str,
    application: &str,
    recorder: &oxiroute_config::RtmpRecorder,
    preflighted_roots: &mut HashSet<PathBuf>,
) -> Result<PreparedRtmpRecorder, ServicePlanError> {
    let invalid = || ServicePlanError::InvalidRecorderPolicy {
        service: service.to_owned(),
        application: application.to_owned(),
        recorder: recorder.name.clone(),
    };
    let path_policy =
        RecordingPathPolicy::new(&recorder.suffix_template, recorder.append_unix_seconds)
            .map_err(|_| invalid())?
            .with_segment_policy(
                match recorder.timezone {
                    oxiroute_config::RtmpRecorderTimezone::Utc => RecordingTimezone::Utc,
                    oxiroute_config::RtmpRecorderTimezone::Iana(ref name) => {
                        RecordingTimezone::Iana(
                            name.parse().expect("validated IANA recorder timezone"),
                        )
                    }
                },
                match recorder.time_basis {
                    oxiroute_config::RtmpRecorderTimeBasis::SegmentStart => {
                        RecordingTimeBasis::SegmentStart
                    }
                    oxiroute_config::RtmpRecorderTimeBasis::SegmentEnd => {
                        RecordingTimeBasis::SegmentEnd
                    }
                },
                match recorder.segment_naming {
                    oxiroute_config::RtmpRecorderSegmentNaming::SafeUnique => {
                        RecordingSegmentNaming::SafeUnique
                    }
                    oxiroute_config::RtmpRecorderSegmentNaming::NginxCompatible => {
                        RecordingSegmentNaming::NginxCompatible
                    }
                },
            );
    let worker_config = RecorderWorkerConfig {
        max_queue_messages: usize::try_from(recorder.max_queue_messages).map_err(|_| invalid())?,
        max_queue_bytes: usize::try_from(recorder.max_queue_bytes).map_err(|_| invalid())?,
        rotation_interval: recorder.rotation_interval_ms.map(Duration::from_millis),
        shutdown_timeout: Duration::from_millis(recorder.shutdown_timeout_ms),
        video_codec: None,
    };
    let store_limits = RecordingStoreLimits {
        max_bytes: recorder.max_storage_bytes,
        max_files: usize::try_from(recorder.max_storage_files).map_err(|_| invalid())?,
        max_active_recorders: usize::try_from(recorder.max_active_recorders)
            .map_err(|_| invalid())?,
    };
    if preflighted_roots.insert(recorder.root_directory.clone()) {
        RecordingStore::preflight(&recorder.root_directory, store_limits).map_err(|_| {
            ServicePlanError::RecorderPreflight {
                service: service.to_owned(),
                application: application.to_owned(),
                recorder: recorder.name.clone(),
            }
        })?;
    }
    Ok(PreparedRtmpRecorder {
        name: recorder.name.clone(),
        start: match recorder.start {
            ConfigRecorderStart::Continuous => RtmpRecorderStart::Continuous,
            ConfigRecorderStart::Manual => RtmpRecorderStart::Manual,
        },
        root_directory: recorder.root_directory.clone(),
        path_policy,
        worker_config,
        store_limits,
    })
}

fn socket_listener_contains(
    listener: std::net::SocketAddr,
    destination: std::net::SocketAddr,
) -> bool {
    if listener.port() != destination.port() {
        return false;
    }
    listener.ip() == destination.ip()
        || listener.ip().is_unspecified() && listener.is_ipv4() == destination.is_ipv4()
}

fn compile_listener(
    listener: &oxiroute_config::Listener,
    http_services: &HashMap<String, Arc<HttpServicePlan>>,
    rtmp_services: &HashMap<String, Arc<RtmpServicePlan>>,
    l4_services: &HashMap<String, Arc<L4ServicePlan>>,
    tls_profiles: &crate::tls::TlsProfilePlanMap,
) -> Result<ServiceSpec, ServicePlanError> {
    let tls = match (listener.protocol, listener.tls_profile.as_deref()) {
        (Protocol::Http, Some(profile)) => {
            Some(Arc::clone(tls_profiles.get(profile).ok_or_else(|| {
                ServicePlanError::UnknownListenerTlsProfile {
                    listener: listener.name.clone(),
                    profile: profile.into(),
                }
            })?))
        }
        (Protocol::Http | Protocol::Tcp | Protocol::Rtmp, None) => None,
        (protocol @ (Protocol::Tcp | Protocol::Rtmp), Some(profile)) => {
            return Err(ServicePlanError::UnexpectedListenerTlsProfile {
                listener: listener.name.clone(),
                protocol,
                profile: profile.into(),
            });
        }
        (Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3, _) => {
            return Err(ServicePlanError::ForwardProxyRuntimeUnavailable {
                listener: listener.name.clone(),
            });
        }
    };
    let kind = match (listener.protocol, listener.service.as_deref()) {
        (Protocol::Http | Protocol::Rtmp | Protocol::Tcp, None) => {
            return Err(ServicePlanError::MissingListenerService {
                listener: listener.name.clone(),
            });
        }
        (Protocol::Http, Some(service)) => {
            ServiceKind::Http(Arc::clone(http_services.get(service).ok_or_else(|| {
                ServicePlanError::UnknownHttpService {
                    listener: listener.name.clone(),
                    service: service.into(),
                }
            })?))
        }
        (Protocol::Tcp, Some(service)) => {
            ServiceKind::Tcp(Arc::clone(l4_services.get(service).ok_or_else(|| {
                ServicePlanError::UnknownL4Service {
                    listener: listener.name.clone(),
                    service: service.into(),
                }
            })?))
        }
        (Protocol::Rtmp, Some(service)) => {
            ServiceKind::Rtmp(Arc::clone(rtmp_services.get(service).ok_or_else(|| {
                ServicePlanError::UnknownRtmpService {
                    listener: listener.name.clone(),
                    service: service.into(),
                }
            })?))
        }
        (Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3, _) => {
            return Err(ServicePlanError::ForwardProxyRuntimeUnavailable {
                listener: listener.name.clone(),
            });
        }
    };
    Ok(ServiceSpec {
        name: listener.name.clone(),
        bind: listener.bind.clone(),
        max_connections: listener.max_connections,
        downstream_timeouts: listener.downstream_timeouts,
        kind,
        tls,
    })
}
