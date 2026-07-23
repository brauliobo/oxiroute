use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crate::{
    CertbotReconciler, HealthBuildError, HealthSupervisor, L4ServicePlan, PoolError, PreparedTls,
    RelayPolicy, RoundRobinPool, Route, RouteError, RouteTable, RuntimeEndpoint, TlsBuildError,
    TlsProfilePlan, TopologySnapshot, health,
    http_action::{
        BearerTokenAccess, FixedResponsePlan, HttpActionPlan, HttpRoutePlan, ProxyActionPlan,
        ProxyPolicyPlan, RedirectPlan, StaticFilesPlan,
    },
    upstream_peer::UpstreamPlan,
};
use http::{Method, Uri, uri::Authority};
use oxiroute_config::{
    Config, HttpProxyPolicy, HttpRouteAction, ListenerBind, Protocol,
    RtmpRecorderStart as ConfigRecorderStart,
};
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, RecorderWorkerConfig, RecordingPathPolicy, RecordingStore,
    RecordingStoreLimits, RtmpApplication as RuntimeRtmpApplication, RtmpCapabilities,
    RtmpRecorderPolicy, RtmpRecorderStart, RtmpRegistry, RtmpServiceRuntime, RtmpSessionPolicy,
};

#[derive(Clone, Debug)]
pub struct ServiceSpec {
    pub name: String,
    pub bind: ListenerBind,
    pub max_connections: Option<u64>,
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
                    Ok(RuntimeRtmpApplication::with_recorders(
                        &application.name,
                        application.live,
                        application.idle_streams,
                        recorders,
                    ))
                })
                .collect::<Result<Vec<_>, ServicePlanError>>()?;
        Ok(RtmpServiceRuntime::new(
            self.service_id.clone(),
            registry,
            self.hub.clone(),
            RtmpSessionPolicy::new(applications),
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
    max_request_body_bytes: Option<u64>,
    route_plans: HashMap<String, Arc<HttpRoutePlan>>,
    upstream_io_timeout: Duration,
    routes: RouteTable,
}

impl HttpServicePlan {
    pub(crate) const fn new(
        max_request_body_bytes: Option<u64>,
        route_plans: HashMap<String, Arc<HttpRoutePlan>>,
        upstream_io_timeout: Duration,
        routes: RouteTable,
    ) -> Self {
        Self {
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

    pub(crate) fn exceeds_body_limit(&self, bytes: u64) -> bool {
        self.max_request_body_bytes
            .is_some_and(|limit| bytes > limit)
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

fn compile_pools(config: &Config) -> Result<CompiledPools, ServicePlanError> {
    let mut pools = HashMap::with_capacity(config.upstream_pools.len());
    let mut health_groups = Vec::new();
    let mut ordered = Vec::with_capacity(config.upstream_pools.len());
    for pool in &config.upstream_pools {
        let endpoints = pool
            .endpoints
            .iter()
            .map(RuntimeEndpoint::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            })?;
        let selector = Arc::new(
            RoundRobinPool::new_named(
                pool.name.clone(),
                endpoints,
                pool.algorithm,
                pool.health_check.is_some(),
            )
            .map_err(|source| ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            })?,
        );
        let tls = crate::tls::prepare_upstream_tls(pool)
            .map_err(|source| ServicePlanError::Tls(Box::new(source)))?
            .map(Arc::new);
        let compiled = Arc::new(UpstreamPlan::new(Arc::clone(&selector), tls));
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
                                    service: service.name.clone(),
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
                .map(BearerTokenAccess::load)
                .transpose()
                .map_err(|_| ServicePlanError::AccessPreflight {
                    service: service.name.clone(),
                    route: route_index,
                })?;
            let action = match &route.action {
                HttpRouteAction::Proxy {
                    upstream_pool,
                    policy,
                } => {
                    compile_proxy_action(&service.name, route_index, upstream_pool, policy, pools)?
                }
                HttpRouteAction::FixedResponse {
                    status,
                    body,
                    headers,
                } => HttpActionPlan::Fixed(FixedResponsePlan::compile(*status, body, headers)),
                HttpRouteAction::Redirect { status, location } => {
                    HttpActionPlan::Redirect(RedirectPlan {
                        status: *status,
                        location: location.clone(),
                    })
                }
                HttpRouteAction::StaticFiles {
                    root_directory,
                    index_files,
                    spa_fallback,
                } => HttpActionPlan::Static(
                    StaticFilesPlan::open(root_directory, index_files, spa_fallback.as_deref())
                        .map_err(|_| ServicePlanError::StaticPreflight {
                            service: service.name.clone(),
                            route: route_index,
                        })?,
                ),
            };
            let route_id = route_index.to_string();
            routes.push(
                Route::new(
                    route.host.clone(),
                    route.path.clone(),
                    methods,
                    route_id.clone(),
                )
                .map_err(|source| ServicePlanError::Route {
                    service: service.name.clone(),
                    route: route_index,
                    source,
                })?,
            );
            route_plans.insert(route_id, Arc::new(HttpRoutePlan { access, action }));
        }
        http_services.insert(
            service.name.clone(),
            Arc::new(HttpServicePlan::new(
                service.max_request_body_bytes,
                route_plans,
                Duration::from_millis(service.upstream_io_timeout_ms),
                RouteTable::new(routes),
            )),
        );
    }
    Ok(http_services)
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
                    connect: Duration::from_millis(service.connect_timeout_ms),
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
    let mut preflighted_roots = HashSet::new();
    let mut services = HashMap::with_capacity(config.rtmp_services.len());
    for service in &config.rtmp_services {
        let mut prepared_applications = Vec::with_capacity(service.applications.len());
        for application in &service.applications {
            let mut prepared_recorders = Vec::with_capacity(application.recorders.len());
            for recorder in &application.recorders {
                let invalid = || ServicePlanError::InvalidRecorderPolicy {
                    service: service.name.clone(),
                    application: application.name.clone(),
                    recorder: recorder.name.clone(),
                };
                let path_policy = RecordingPathPolicy::new(
                    &recorder.suffix_template,
                    recorder.append_unix_seconds,
                )
                .map_err(|_| invalid())?;
                let worker_config = RecorderWorkerConfig {
                    max_queue_messages: usize::try_from(recorder.max_queue_messages)
                        .map_err(|_| invalid())?,
                    max_queue_bytes: usize::try_from(recorder.max_queue_bytes)
                        .map_err(|_| invalid())?,
                    rotation_interval: recorder.rotation_interval_ms.map(Duration::from_millis),
                    shutdown_timeout: Duration::from_millis(recorder.shutdown_timeout_ms),
                    video_codec: None,
                };
                let store_limits = RecordingStoreLimits {
                    max_bytes: recorder.max_storage_bytes,
                    max_files: usize::try_from(recorder.max_storage_files)
                        .map_err(|_| invalid())?,
                    max_active_recorders: usize::try_from(recorder.max_active_recorders)
                        .map_err(|_| invalid())?,
                };
                if preflighted_roots.insert(recorder.root_directory.clone()) {
                    RecordingStore::preflight(&recorder.root_directory, store_limits).map_err(
                        |_| ServicePlanError::RecorderPreflight {
                            service: service.name.clone(),
                            application: application.name.clone(),
                            recorder: recorder.name.clone(),
                        },
                    )?;
                }
                prepared_recorders.push(PreparedRtmpRecorder {
                    name: recorder.name.clone(),
                    start: match recorder.start {
                        ConfigRecorderStart::Continuous => RtmpRecorderStart::Continuous,
                        ConfigRecorderStart::Manual => RtmpRecorderStart::Manual,
                    },
                    root_directory: recorder.root_directory.clone(),
                    path_policy,
                    worker_config,
                    store_limits,
                });
            }
            prepared_applications.push(PreparedRtmpApplication {
                name: application.name.clone(),
                live: application.live,
                idle_streams: application.idle_streams,
                recorders: prepared_recorders,
            });
        }
        services.insert(
            service.name.clone(),
            Arc::new(RtmpServicePlan {
                service_id: service.name.clone(),
                hub: LiveHub::new(LiveHubLimits::default()),
                applications: prepared_applications,
            }),
        );
    }
    Ok(services)
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
        kind,
        tls,
    })
}
