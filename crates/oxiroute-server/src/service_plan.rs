use std::{
    collections::{HashMap, HashSet},
    fs,
    net::ToSocketAddrs,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use crate::{
    CertbotReconciler, ForwardHttp1ServicePlan, ForwardHttp2ServicePlan, HealthBuildError,
    HealthSupervisor, L4ServicePlan, PassiveFailurePolicy, PoolError, PreparedTls, RelayPolicy,
    RoundRobinPool, Route, RouteError, RouteTable, RuntimeEndpoint, TlsBuildError, TlsProfilePlan,
    TopologySnapshot, health,
    http_action::{
        AccessLog, CachePurgeAccess, DiskBackend, FixedResponsePlan, HttpActionPlan,
        HttpCacheBackend, HttpCachePlan, HttpGzipPlan, HttpRoutePlan, ProxyActionPlan,
        ProxyPolicyPlan, RedirectPlan, RouteAccess, RoutePolicyPlan, StaticFilesPlan,
    },
    routing::RuntimeServer,
    upstream_peer::UpstreamPlan,
};
use http::{Method, Uri, uri::Authority};
use oxiroute_cache::{Cache, CacheConfig, CacheTimeline, DiskCache, DiskCacheConfig};
use oxiroute_config::{
    CacheAuthorizationPolicy, CacheKeyComponent, CachePurgeAuthorization, CacheSetCookiePolicy,
    CacheStore, CacheVaryPolicy, Config, DnsResolutionPolicy, HttpCachePolicy, HttpProxyPolicy,
    HttpRoute as ConfigHttpRoute, HttpRouteAction, ListenerBind, Protocol,
    RtmpAccessPolicy as ConfigRtmpAccessPolicy,
    RtmpExecFilesystemPolicy as ConfigExecFilesystemPolicy, RtmpExecMode as ConfigExecMode,
    RtmpExecNetworkPolicy as ConfigExecNetworkPolicy, RtmpExecTrigger as ConfigExecTrigger,
    RtmpRecorderStart as ConfigRecorderStart, UdpPolicy,
};
use oxiroute_rtmp::{
    DashOutputConfig, DashSegmentNaming, ExecEnvironment, ExecFilesystemPolicy, ExecLimits,
    ExecMode, ExecNetworkPolicy, ExecProfile, ExecTrigger, HlsFragmentNaming, HlsKeyConfig,
    HlsOutputConfig, HlsVariant, LiveHub, LiveHubLimits, MediaApplication, MediaCatalog,
    MediaStore, MediaStoreLimits, RecorderMediaMask, RecorderWorkerConfig, RecordingPathPolicy,
    RecordingSegmentNaming, RecordingStore, RecordingStoreLimits, RecordingTimeBasis,
    RecordingTimezone, RtmpAccessAction, RtmpAccessPolicy as RuntimeRtmpAccessPolicy,
    RtmpAccessRule, RtmpApplication as RuntimeRtmpApplication, RtmpAutoPushConfig,
    RtmpCallbackEndpoint, RtmpCallbackMethod, RtmpCallbackPolicy, RtmpCapabilities,
    RtmpClientOptions, RtmpCredential, RtmpNetwork, RtmpOutboundPolicy, RtmpPullTarget,
    RtmpPushApplication, RtmpPushTarget, RtmpRecorderPolicy, RtmpRecorderStart, RtmpRegistry,
    RtmpRelayConfig, RtmpRtmpsMode, RtmpServiceRuntime, RtmpSessionCeilings, RtmpSessionLimits,
    RtmpSessionPolicy, RtmpTokenPolicy, RtmpTransport, VodApplication, VodCatalog, VodLimits,
    VodSourceDefinition,
};

static DISK_BACKEND_REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<DiskBackend>>>> =
    OnceLock::new();

#[derive(Clone, Debug)]
pub struct ServiceSpec {
    pub name: String,
    pub bind: ListenerBind,
    pub max_connections: Option<u64>,
    pub downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy,
    pub proxy_protocol: Option<oxiroute_config::ProxyProtocolPolicy>,
    pub kind: ServiceKind,
    pub tls: Option<Arc<TlsProfilePlan>>,
}

#[derive(Clone, Debug)]
pub enum ServiceKind {
    ForwardHttp1(Arc<ForwardHttp1ServicePlan>),
    ForwardHttp2(Arc<ForwardHttp2ServicePlan>),
    ForwardHttp3(Arc<ForwardHttp1ServicePlan>),
    Http3(Arc<HttpServicePlan>),
    Http(Arc<HttpServicePlan>),
    Rtmp(Arc<RtmpServicePlan>),
    Tcp(Arc<L4ServicePlan>),
    Udp(Arc<L4ServicePlan>),
}

impl ServiceKind {
    #[must_use]
    pub const fn protocol(&self) -> &'static str {
        match self {
            Self::ForwardHttp1(_) => "forward_http1",
            Self::ForwardHttp2(_) => "forward_http2",
            Self::ForwardHttp3(_) => "forward_http3",
            Self::Http3(_) => "http3",
            Self::Http(_) => "http",
            Self::Rtmp(_) => "rtmp",
            Self::Tcp(_) => "tcp",
            Self::Udp(_) => "udp",
        }
    }
}

pub struct RtmpServicePlan {
    service_id: String,
    outbound_chunk_size: u32,
    inbound_limits: RtmpSessionLimits,
    hub: LiveHub,
    callbacks: RtmpCallbackPolicy,
    access_log: Option<Arc<AccessLog>>,
    applications: Vec<PreparedRtmpApplication>,
    vod_catalog: Arc<VodCatalog>,
    media_catalog: Arc<MediaCatalog>,
    auto_push: Option<RtmpAutoPushConfig>,
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

    /// Writes one bounded RTMP access event when service access logging is enabled.
    ///
    /// # Errors
    ///
    /// Returns the nonblocking access-log queue error when it is full or stopped.
    pub fn write_access_event(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.access_log
            .as_ref()
            .map_or(Ok(()), |access_log| access_log.write(event))
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
                    )
                    .with_pull_targets(application.pull_targets.clone())
                    .with_vod(application.vod.clone())
                    .with_media(application.media.clone())
                    .with_exec_profiles(application.exec_profiles.clone())
                    .with_callbacks(application.callbacks.clone())
                    .with_authorization(
                        application.publish_policy.clone(),
                        application.play_policy.clone(),
                        application.session_limits,
                    ))
                })
                .collect::<Result<Vec<_>, ServicePlanError>>()?;
        let mut runtime = RtmpServiceRuntime::new(
            self.service_id.clone(),
            registry,
            self.hub.clone(),
            RtmpSessionPolicy::with_session_limits(
                applications,
                self.outbound_chunk_size,
                self.inbound_limits,
            ),
        )
        .with_callbacks(self.callbacks.clone());
        if let Some(auto_push) = self.auto_push.clone() {
            runtime = runtime.with_auto_push(auto_push).map_err(|_| {
                ServicePlanError::AutoPushUnavailable {
                    service: self.service_id.clone(),
                }
            })?;
        }
        Ok(runtime)
    }

    #[must_use]
    pub fn vod_catalog(&self) -> Arc<VodCatalog> {
        Arc::clone(&self.vod_catalog)
    }

    #[must_use]
    pub fn media_catalog(&self) -> Arc<MediaCatalog> {
        Arc::clone(&self.media_catalog)
    }

    #[must_use]
    pub fn vod_applications(&self) -> Vec<Arc<VodApplication>> {
        self.applications
            .iter()
            .filter_map(|application| application.vod.clone())
            .collect()
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

    #[must_use]
    pub fn auto_push_enabled(&self) -> bool {
        self.auto_push.is_some()
    }
}

#[derive(Clone)]
struct PreparedRtmpApplication {
    name: String,
    live: bool,
    idle_streams: bool,
    publish_policy: RuntimeRtmpAccessPolicy,
    play_policy: RuntimeRtmpAccessPolicy,
    session_limits: RtmpSessionCeilings,
    hub: LiveHub,
    push_targets: Vec<RtmpPushTarget>,
    pull_targets: Vec<RtmpPullTarget>,
    callbacks: RtmpCallbackPolicy,
    vod: Option<Arc<VodApplication>>,
    media: Option<Arc<MediaApplication>>,
    exec_profiles: Vec<ExecProfile>,
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
    automatic_response_headers: bool,
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
            automatic_response_headers: true,
            gzip: None,
            max_request_body_bytes,
            route_plans,
            upstream_io_timeout,
            routes,
        }
    }

    fn with_http_policy(
        automatic_response_headers: bool,
        max_request_body_bytes: Option<u64>,
        route_plans: HashMap<String, Arc<HttpRoutePlan>>,
        upstream_io_timeout: Duration,
        routes: RouteTable,
        gzip: Option<Arc<HttpGzipPlan>>,
        access_log: Option<Arc<AccessLog>>,
    ) -> Self {
        Self {
            access_log,
            automatic_response_headers,
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

    #[must_use]
    pub const fn automatic_response_headers(&self) -> bool {
        self.automatic_response_headers
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
    #[error("HTTP/3 upstream pool `{pool}` cannot be prepared: {source}")]
    H3Upstream {
        pool: String,
        source: Box<crate::H3UpstreamBuildError>,
    },
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
    #[error("UDP listener `{listener}` references unknown service `{service}`")]
    UnknownUdpService { listener: String, service: String },
    #[error("RTMP listener `{listener}` references unknown service `{service}`")]
    UnknownRtmpService { listener: String, service: String },
    #[error("forward proxy runtime is not integrated for listener `{listener}`")]
    ForwardProxyRuntimeUnavailable { listener: String },
    #[error("forward proxy service `{service}` failed runtime preflight: {source}")]
    ForwardProxyPreflight {
        service: String,
        source: crate::forward_proxy::ForwardPlanError,
    },
    #[error("forward proxy listener `{listener}` references unknown service `{service}`")]
    UnknownForwardProxyService { listener: String, service: String },
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
        "RTMP exec profile `{profile}` in application `{application}` of service `{service}` has an invalid runtime policy"
    )]
    InvalidExecProfile {
        service: String,
        application: String,
        profile: String,
    },
    #[error(
        "RTMP HLS output in application `{application}` of service `{service}` failed media-root preflight"
    )]
    HlsPreflight { service: String, application: String },
    #[error(
        "RTMP DASH output in application `{application}` of service `{service}` failed media-root preflight"
    )]
    DashPreflight { service: String, application: String },
    #[error("RTMP auto-push for service `{service}` is unavailable")]
    AutoPushUnavailable { service: String },
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
    #[error(
        "RTMP pull target {target} in application `{application}` of service `{service}` cannot be resolved safely"
    )]
    RtmpPullResolution {
        service: String,
        application: String,
        target: usize,
    },
    #[error(
        "RTMP VOD source `{source_name}` in application `{application}` of service `{service}` failed secure preflight"
    )]
    RtmpVodPreflight {
        service: String,
        application: String,
        source_name: String,
    },
    #[error("RTMP callback `{field}` in {scope} of service `{service}` failed secure preflight")]
    RtmpCallbackPreflight {
        service: String,
        scope: String,
        field: &'static str,
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
    pub rtmp_vod_catalog: Arc<VodCatalog>,
    pub rtmp_media_catalog: Arc<MediaCatalog>,
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
    runtime_plan_with_passive_failure_policy(config, PassiveFailurePolicy::default())
}

/// Compiles one immutable runtime generation with an explicit passive endpoint policy.
///
/// # Errors
///
/// Returns an error when a pool, route, reference, or health probe cannot be compiled, including
/// when the passive policy exceeds its runtime bounds.
pub fn runtime_plan_with_passive_failure_policy(
    config: &Config,
    passive_policy: PassiveFailurePolicy,
) -> Result<RuntimePlan, ServicePlanError> {
    reject_unimplemented_runtime_policies(config)?;
    let mut config = config.clone();
    oxiroute_config::validate_config(&mut config)
        .map_err(|source| ServicePlanError::InvalidConfig(Box::new(source)))?;
    let tls = crate::tls::prepare_tls(&config)
        .map_err(|source| ServicePlanError::Tls(Box::new(source)))?;
    let pools = compile_pools(&config, passive_policy)?;
    let http_services = compile_http_services(&config, &pools.by_name)?;
    let forward_services = compile_forward_proxy_services(&config)?;
    let rtmp_services = compile_rtmp_services(&config)?;
    let l4_services = compile_l4_services(&config, &pools.by_name)?;

    let services = config
        .listeners
        .iter()
        .map(|listener| {
            compile_listener(
                listener,
                &http_services,
                &forward_services,
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
        ServiceKind::ForwardHttp1(_)
        | ServiceKind::ForwardHttp2(_)
        | ServiceKind::ForwardHttp3(_)
        | ServiceKind::Http3(_)
        | ServiceKind::Http(_)
        | ServiceKind::Tcp(_)
        | ServiceKind::Udp(_) => None,
    });
    let rtmp_capabilities = RtmpCapabilities {
        live_ingest: active_rtmp_services.clone().next().is_some(),
        manual_recording: active_rtmp_services
            .clone()
            .any(RtmpServicePlan::manual_recording),
    };
    let rtmp_recording_supported = active_rtmp_services.any(RtmpServicePlan::recording_supported);
    let rtmp_vod_catalog = VodCatalog::from_applications(
        rtmp_services
            .values()
            .flat_map(|service| service.vod_applications()),
    );
    let rtmp_media_catalog = MediaCatalog::merge(
        rtmp_services
            .values()
            .map(|service| service.media_catalog()),
    );
    Ok(RuntimePlan {
        max_connections: config.max_connections,
        services,
        health_supervisor,
        pools: pools.ordered,
        rtmp_capabilities,
        rtmp_recording_supported,
        rtmp_vod_catalog,
        rtmp_media_catalog: Arc::new(rtmp_media_catalog),
        tls,
        topology,
    })
}

fn compile_forward_proxy_services(
    config: &Config,
) -> Result<HashMap<String, Arc<ForwardHttp1ServicePlan>>, ServicePlanError> {
    let mut cache_backends = HashMap::new();
    config
        .forward_proxy_services
        .iter()
        .map(|service| {
            let cache = compile_cache_policy(
                &service.name,
                0,
                service.header_policy.cache.as_deref(),
                false,
                &[],
                &config.cache_stores,
                &mut cache_backends,
                false,
            )?;
            let plan =
                ForwardHttp1ServicePlan::compile_with_cache(service, cache).map_err(|source| {
                    ServicePlanError::ForwardProxyPreflight {
                        service: service.name.clone(),
                        source,
                    }
                })?;
            Ok((service.name.clone(), Arc::new(plan)))
        })
        .collect()
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
fn compile_pools(
    config: &Config,
    passive_policy: PassiveFailurePolicy,
) -> Result<CompiledPools, ServicePlanError> {
    let protected_addresses: Arc<[std::net::SocketAddr]> = config
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
            RoundRobinPool::new_named_servers_with_policy(
                pool.name.clone(),
                servers,
                pool.algorithm.clone(),
                pool.health_check.as_ref().map(|health| health.startup),
                pool.queue_timeout_ms.map(Duration::from_millis),
                passive_policy,
            )
            .map_err(|source| ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            })?,
        );
        let tls = crate::tls::prepare_upstream_tls(pool)
            .map_err(|source| ServicePlanError::Tls(Box::new(source)))?
            .map(Arc::new);
        let h3 = crate::H3UpstreamPlan::from_pool(pool)
            .map_err(|source| ServicePlanError::H3Upstream {
                pool: pool.name.clone(),
                source: Box::new(source),
            })?
            .map(Arc::new);
        let compiled = Arc::new(UpstreamPlan::with_http_policy(
            Arc::clone(&selector),
            tls,
            pool.connect_timeout_ms.map(Duration::from_millis),
            pool.server_timeout_ms.map(Duration::from_millis),
            pool.connection_reuse,
            pool.http_versions.min,
            h3,
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
    let mut cache_backends = HashMap::new();
    for service in &config.http_services {
        let mut routes = Vec::with_capacity(service.routes.len());
        let mut route_plans = HashMap::with_capacity(service.routes.len());
        for (route_index, route) in service.routes.iter().enumerate() {
            let (compiled_route, plan) = compile_http_route(
                &service.name,
                route_index,
                route,
                pools,
                &config.cache_stores,
                &mut cache_backends,
                service.gzip.is_some(),
            )?;
            routes.push(compiled_route);
            route_plans.insert(route_index.to_string(), plan);
        }
        http_services.insert(
            service.name.clone(),
            Arc::new(HttpServicePlan::with_http_policy(
                service.automatic_response_headers,
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
    cache_stores: &[CacheStore],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    has_gzip: bool,
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
        } => compile_proxy_action(
            service,
            route_index,
            upstream_pool,
            policy,
            route.access_policy.is_some(),
            pools,
            cache_stores,
            cache_backends,
            has_gzip,
        )?,
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
        | oxiroute_config::HttpPathSelector::Exact { value }
        | oxiroute_config::HttpPathSelector::AsciiCaseInsensitiveExact { value } => value,
    }
}

fn reject_unimplemented_runtime_policies(config: &Config) -> Result<(), ServicePlanError> {
    let unavailable = |policy| ServicePlanError::RuntimePolicyUnavailable { policy };
    for service in &config.http_services {
        for route in &service.routes {
            if route.policy.response_buffering && route.policy.max_request_body_bytes.is_none() {
                return Err(unavailable(
                    "http_services[].routes[].policy.unbounded_response_buffering",
                ));
            }
            if route.policy.request_buffering && route.policy.max_request_body_bytes.is_none() {
                return Err(unavailable(
                    "http_services[].routes[].policy.unbounded_request_buffering",
                ));
            }
        }
    }
    Ok(())
}

fn open_shared_disk_backend(
    root: &std::path::Path,
    config: DiskCacheConfig,
) -> Result<Arc<DiskBackend>, ()> {
    let registry = DISK_BACKEND_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = registry.get(root).and_then(Weak::upgrade) {
        return (existing.disk_config() == &config)
            .then_some(existing)
            .ok_or(());
    }
    let cache = Arc::new(DiskCache::open(root, config).map_err(|_| ())?);
    let backend = Arc::new(DiskBackend::new(cache));
    registry.insert(root.to_owned(), Arc::downgrade(&backend));
    Ok(backend)
}

#[expect(
    clippy::too_many_arguments,
    reason = "proxy compilation carries route errors and shared cache-generation state"
)]
fn compile_proxy_action(
    service: &str,
    route: usize,
    upstream_pool: &str,
    policy: &HttpProxyPolicy,
    has_access_policy: bool,
    pools: &HashMap<String, Arc<UpstreamPlan>>,
    cache_stores: &[CacheStore],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    has_gzip: bool,
) -> Result<HttpActionPlan, ServicePlanError> {
    let pool = pools
        .get(upstream_pool)
        .ok_or_else(|| ServicePlanError::UnknownHttpPool {
            service: service.into(),
            route,
            pool: upstream_pool.into(),
        })?;
    let cache = compile_cache_policy(
        service,
        route,
        policy.cache.as_deref(),
        has_access_policy,
        &policy.request_headers,
        cache_stores,
        cache_backends,
        has_gzip,
    )?;
    Ok(HttpActionPlan::Proxy(ProxyActionPlan {
        pool: Arc::clone(pool),
        policy: ProxyPolicyPlan::compile_with_cache(policy, cache),
    }))
}

#[allow(clippy::too_many_arguments)]
#[expect(
    clippy::too_many_lines,
    reason = "supported cache policy checks and runtime construction stay fail-closed together"
)]
fn compile_cache_policy(
    service: &str,
    route: usize,
    policy: Option<&HttpCachePolicy>,
    has_access_policy: bool,
    request_headers: &[oxiroute_config::HttpRequestHeaderMutation],
    stores: &[CacheStore],
    cache_backends: &mut HashMap<String, Arc<HttpCacheBackend>>,
    has_gzip: bool,
) -> Result<Option<Arc<HttpCachePlan>>, ServicePlanError> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    let unavailable = |name| ServicePlanError::RuntimePolicyUnavailable { policy: name };
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
    let cache = if let Some(cache) = cache_backends.get(&policy.store) {
        Arc::clone(cache)
    } else {
        let store = stores.iter().find(|store| match store {
            CacheStore::Memory { name, .. } | CacheStore::Disk { name, .. } => {
                name == &policy.store
            }
        });
        let Some(store) = store else {
            return Err(ServicePlanError::CacheRuntimeUnavailable {
                service: service.into(),
                route,
            });
        };
        let to_usize = |value: u64| usize::try_from(value).map_err(|_| unavailable("cache bounds"));
        let make_memory_config = |max_bytes: u64,
                                  max_entries: u64,
                                  max_object_bytes: u64,
                                  max_header_bytes: u64,
                                  max_key_bytes: u64,
                                  max_tag_bytes: u64,
                                  max_tags_per_object: u64,
                                  max_in_flight_fills: u64,
                                  max_followers_per_fill: u64|
         -> Result<CacheConfig, ServicePlanError> {
            Ok(CacheConfig {
                max_entries: to_usize(max_entries)?,
                max_total_bytes: to_usize(max_bytes)?,
                max_object_bytes: to_usize(max_object_bytes)?,
                max_header_bytes: to_usize(max_header_bytes)?,
                max_header_fields: 256,
                max_body_bytes: to_usize(max_object_bytes)?,
                max_key_bytes: to_usize(max_key_bytes)?,
                max_vary_fields: 32,
                max_tags_per_entry: to_usize(max_tags_per_object)?,
                max_tag_bytes: to_usize(max_tag_bytes)?,
                max_in_flight: to_usize(max_in_flight_fills)?,
                max_followers_per_fill: to_usize(max_followers_per_fill)?,
                max_heuristic_freshness: Duration::from_secs(24 * 60 * 60),
            })
        };
        let cache = match store {
            CacheStore::Memory {
                max_bytes,
                max_entries,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
                ..
            } => {
                let cache_config = make_memory_config(
                    *max_bytes,
                    *max_entries,
                    *max_object_bytes,
                    *max_header_bytes,
                    *max_key_bytes,
                    *max_tag_bytes,
                    *max_tags_per_object,
                    *max_in_flight_fills,
                    *max_followers_per_fill,
                )?;
                Arc::new(HttpCacheBackend::Memory(Arc::new(
                    Cache::new(cache_config).map_err(|_| {
                        unavailable("http_services[].routes[].action.policy.cache.memory")
                    })?,
                )))
            }
            CacheStore::Disk {
                root_directory,
                max_bytes,
                max_files,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
                ..
            } => {
                let memory = make_memory_config(
                    *max_bytes,
                    *max_files,
                    *max_object_bytes,
                    *max_header_bytes,
                    *max_key_bytes,
                    *max_tag_bytes,
                    *max_tags_per_object,
                    *max_in_flight_fills,
                    *max_followers_per_fill,
                )?;
                let disk_config = DiskCacheConfig {
                    memory,
                    max_disk_bytes: *max_bytes,
                    max_disk_files: to_usize(*max_files)?,
                    max_record_bytes: to_usize(*max_bytes)?,
                };
                let backend =
                    open_shared_disk_backend(root_directory, disk_config).map_err(|()| {
                        unavailable("http_services[].routes[].action.policy.cache.disk")
                    })?;
                Arc::new(HttpCacheBackend::Disk(backend))
            }
        };
        cache_backends.insert(policy.store.clone(), Arc::clone(&cache));
        cache
    };
    let timeline = CacheTimeline::new(
        policy.use_origin_cache_control,
        Duration::from_millis(policy.default_ttl_ms),
        policy.status_ttls.iter().map(|status_ttl| {
            (
                http::StatusCode::from_u16(status_ttl.status).expect("validated cache status TTL"),
                Duration::from_millis(status_ttl.ttl_ms),
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
        http::HeaderName::from_bytes(tags.response_header.as_bytes())
            .expect("validated cache surrogate header")
    });
    let surrogate_limits = policy
        .surrogate_tags
        .as_ref()
        .map(|tags| {
            Ok::<_, ServicePlanError>((
                usize::try_from(tags.max_tags).map_err(|_| unavailable("cache tag bounds"))?,
                usize::try_from(tags.max_tag_bytes).map_err(|_| unavailable("cache tag bounds"))?,
            ))
        })
        .transpose()?;
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
        timeline,
        methods,
        revalidate: policy.revalidate,
        surrogate_header,
        surrogate_limits,
        purge_access,
    })))
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
                service.proxy_protocol,
                service.udp.unwrap_or_else(UdpPolicy::default),
            )),
        );
    }
    Ok(l4_services)
}

#[allow(clippy::too_many_lines)]
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
    let mut media_stores = HashMap::<PathBuf, Arc<MediaStore>>::new();
    let mut services = HashMap::with_capacity(config.rtmp_services.len());
    for service in &config.rtmp_services {
        let outbound_policy = compile_rtmp_outbound_policy(&service.outbound_policy);
        let inbound_limits = RtmpSessionLimits::default()
            .with_max_inbound_message_size(
                usize::try_from(service.max_inbound_message_size).map_err(|_| {
                    ServicePlanError::RuntimePolicyUnavailable {
                        policy: "rtmp_services[].max_inbound_message_size",
                    }
                })?,
            )
            .with_window_ack_size(service.ack_window_size);
        let auto_push = compile_rtmp_auto_push(&service.name, &service.auto_push)?;
        let callbacks =
            compile_rtmp_callbacks(&service.name, None, &service.callbacks, &outbound_policy)?;
        let mut prepared_applications = Vec::with_capacity(service.applications.len());
        for application in &service.applications {
            let (hub, _, _) = compile_rtmp_fanout(application)?;
            let publish_policy = compile_rtmp_access_policy(
                &service.name,
                &application.name,
                "publish",
                &application.publish,
            )?;
            let play_policy = compile_rtmp_access_policy(
                &service.name,
                &application.name,
                "play",
                &application.play,
            )?;
            let session_limits = RtmpSessionCeilings::new(
                usize::try_from(application.limits.max_connections).map_err(|_| {
                    ServicePlanError::RuntimePolicyUnavailable {
                        policy: "rtmp_services[].applications[].limits.max_connections",
                    }
                })?,
                usize::try_from(application.limits.max_publishers).map_err(|_| {
                    ServicePlanError::RuntimePolicyUnavailable {
                        policy: "rtmp_services[].applications[].limits.max_publishers",
                    }
                })?,
                usize::try_from(application.limits.max_viewers).map_err(|_| {
                    ServicePlanError::RuntimePolicyUnavailable {
                        policy: "rtmp_services[].applications[].limits.max_viewers",
                    }
                })?,
            );
            let push_targets = compile_rtmp_push_targets(
                &service.name,
                application,
                &listener_addresses,
                &outbound_policy,
                &application.relay,
            )?;
            let pull_targets = compile_rtmp_pull_targets(
                &service.name,
                application,
                &listener_addresses,
                &outbound_policy,
                &application.relay,
            )?;
            let callbacks = compile_rtmp_callbacks(
                &service.name,
                Some(&application.name),
                &application.callbacks,
                &outbound_policy,
            )?;
            let vod = compile_rtmp_vod(&service.name, application, &outbound_policy)?;
            let hls = compile_rtmp_hls(
                &service.name,
                application,
                &mut media_stores,
            )?;
            let dash = compile_rtmp_dash(&service.name, application, &mut media_stores)?;
            let media = match (hls, dash) {
                (None, None) => None,
                (Some(hls), None) => Some(hls),
                (None, Some(dash)) => Some(Arc::new(MediaApplication::new(None).with_dash(Some(dash)))),
                (Some(hls), Some(dash)) => {
                    Some(Arc::new((*hls).clone().with_dash(Some(dash))))
                }
            };
            let exec_profiles = service
                .exec_profiles
                .iter()
                .filter(|profile| profile.application == application.name)
                .map(|profile| compile_rtmp_exec_profile(&service.name, application, profile))
                .collect::<Result<Vec<_>, _>>()?;
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
                publish_policy,
                play_policy,
                session_limits,
                hub,
                push_targets,
                pull_targets,
                callbacks,
                vod,
                media,
                exec_profiles,
                recorders: prepared_recorders,
            });
        }
        let service_hub = prepared_applications.first().map_or_else(
            || LiveHub::new(LiveHubLimits::default()),
            |application| application.hub.clone(),
        );
        let vod_catalog = VodCatalog::from_applications(
            prepared_applications
                .iter()
                .filter_map(|application| application.vod.clone()),
        );
        let media_catalog = MediaCatalog::from_applications(
            prepared_applications.iter().filter_map(|application| {
                application.media.clone().map(|media| {
                    (service.name.clone(), application.name.clone(), media)
                })
            }),
        );
        services.insert(
            service.name.clone(),
            Arc::new(RtmpServicePlan {
                service_id: service.name.clone(),
                outbound_chunk_size: service.outbound_chunk_size,
                inbound_limits,
                hub: service_hub,
                callbacks,
                access_log: AccessLog::open(&service.name, service.access_log.as_ref())
                    .map_err(|_| ServicePlanError::AccessLogPreflight {
                        service: service.name.clone(),
                    })?
                    .map(Arc::new),
                applications: prepared_applications,
                vod_catalog,
                media_catalog: Arc::new(media_catalog),
                auto_push,
            }),
        );
    }
    Ok(services)
}

fn compile_rtmp_access_policy(
    service: &str,
    application: &str,
    operation: &'static str,
    policy: &ConfigRtmpAccessPolicy,
) -> Result<RuntimeRtmpAccessPolicy, ServicePlanError> {
    let unavailable = || ServicePlanError::RuntimePolicyUnavailable {
        policy: match operation {
            "publish" => "rtmp_services[].applications[].publish",
            "play" => "rtmp_services[].applications[].play",
            _ => unreachable!("RTMP access operation is closed"),
        },
    };
    let rules = policy
        .rules
        .iter()
        .map(|rule| {
            let action = match rule.action {
                oxiroute_config::RtmpAclAction::Allow => RtmpAccessAction::Allow,
                oxiroute_config::RtmpAclAction::Deny => RtmpAccessAction::Deny,
            };
            let network = RtmpNetwork::parse(&rule.network).ok_or_else(unavailable)?;
            Ok(RtmpAccessRule::new(action, network))
        })
        .collect::<Result<Vec<_>, ServicePlanError>>()?;
    let token = match policy.token.as_ref() {
        Some(token) => Some(
            RtmpTokenPolicy::stream_query(token.parameter.as_str(), token.secret.as_bytes())
                .ok_or_else(unavailable)?,
        ),
        None => None,
    };
    let _ = (service, application);
    Ok(RuntimeRtmpAccessPolicy::new(rules, token))
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
    outbound_policy: &RtmpOutboundPolicy,
    relay: &oxiroute_config::RtmpRelayPolicy,
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
            let transport = compile_rtmp_transport(target.scheme);
            outbound_policy
                .validate_resolved(target.host.as_str(), &addresses)
                .and_then(|()| outbound_policy.validate_transport(transport))
                .map_err(|_| resolution())?;
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
                host: target.host.clone(),
                transport,
                application: if target.application == "$name" {
                    RtmpPushApplication::StreamName
                } else {
                    RtmpPushApplication::Exact(target.application.clone())
                },
                stream_name: target.stream_name.clone(),
                options: compile_rtmp_client_options(
                    target.tc_url.clone(),
                    target.flash_version.clone(),
                    target.credentials.as_ref(),
                    resolution,
                )?,
                config: RtmpRelayConfig {
                    max_queue_messages: usize::try_from(relay.max_queue_messages)
                        .map_err(|_| resolution())?,
                    max_queue_bytes: usize::try_from(relay.max_queue_bytes)
                        .map_err(|_| resolution())?,
                    buffer_duration: Duration::from_millis(relay.buffer_ms),
                    connect_timeout: Duration::from_millis(relay.connect_timeout_ms),
                    handshake_timeout: Duration::from_millis(relay.handshake_timeout_ms),
                    reconnect_interval: Duration::from_millis(relay.push_reconnect_ms),
                    max_chain_depth: outbound_policy.max_chain_depth,
                },
            })
        })
        .collect()
}

fn compile_rtmp_pull_targets(
    service: &str,
    application: &oxiroute_config::RtmpApplication,
    listener_addresses: &[std::net::SocketAddr],
    outbound_policy: &RtmpOutboundPolicy,
    relay: &oxiroute_config::RtmpRelayPolicy,
) -> Result<Vec<RtmpPullTarget>, ServicePlanError> {
    application
        .pull_targets
        .iter()
        .enumerate()
        .map(|(target_index, target)| {
            let resolution = || ServicePlanError::RtmpPullResolution {
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
            let transport = compile_rtmp_transport(target.scheme);
            outbound_policy
                .validate_resolved(target.host.as_str(), &addresses)
                .and_then(|()| outbound_policy.validate_transport(transport))
                .map_err(|_| resolution())?;
            if addresses.iter().any(|destination| {
                listener_addresses
                    .iter()
                    .any(|listener| socket_listener_contains(*listener, *destination))
            }) {
                return Err(resolution());
            }
            Ok(RtmpPullTarget {
                address: addresses[0],
                host: target.host.clone(),
                transport,
                source_application: target.application.clone(),
                source_stream_name: target.stream_name.clone(),
                local_application: application.name.clone(),
                local_stream_name: target.stream_name.clone(),
                options: compile_rtmp_client_options(
                    target.tc_url.clone(),
                    target.flash_version.clone(),
                    target.credentials.as_ref(),
                    resolution,
                )?,
                config: RtmpRelayConfig {
                    max_queue_messages: usize::try_from(relay.max_queue_messages)
                        .map_err(|_| resolution())?,
                    max_queue_bytes: usize::try_from(relay.max_queue_bytes)
                        .map_err(|_| resolution())?,
                    buffer_duration: Duration::from_millis(relay.buffer_ms),
                    connect_timeout: Duration::from_millis(relay.connect_timeout_ms),
                    handshake_timeout: Duration::from_millis(relay.handshake_timeout_ms),
                    reconnect_interval: Duration::from_millis(relay.pull_reconnect_ms),
                    max_chain_depth: outbound_policy.max_chain_depth,
                },
            })
        })
        .collect()
}

fn compile_rtmp_outbound_policy(
    policy: &oxiroute_config::RtmpOutboundPolicy,
) -> RtmpOutboundPolicy {
    RtmpOutboundPolicy {
        allow_domains: policy.allow_domains.clone(),
        deny_domains: policy.deny_domains.clone(),
        allow_cidrs: policy.allow_cidrs.clone(),
        deny_cidrs: policy.deny_cidrs.clone(),
        deny_private: policy.deny_private,
        rtmps: match policy.rtmps {
            oxiroute_config::RtmpRtmpsPolicy::Disabled => RtmpRtmpsMode::Disabled,
            oxiroute_config::RtmpRtmpsPolicy::Allowed => RtmpRtmpsMode::Allowed,
            oxiroute_config::RtmpRtmpsPolicy::Required => RtmpRtmpsMode::Required,
        },
        max_chain_depth: policy.max_chain_depth,
    }
}

fn compile_rtmp_auto_push(
    service: &str,
    policy: &oxiroute_config::RtmpAutoPushPolicy,
) -> Result<Option<RtmpAutoPushConfig>, ServicePlanError> {
    if !policy.enabled {
        return Ok(None);
    }
    let unavailable = || ServicePlanError::AutoPushUnavailable {
        service: service.to_owned(),
    };
    Ok(Some(RtmpAutoPushConfig {
        enabled: true,
        socket_dir: policy.socket_dir.clone(),
        secret_file: policy.secret_file.clone(),
        reconnect_interval: Duration::from_millis(policy.reconnect_ms),
        connect_timeout: Duration::from_millis(policy.connect_timeout_ms),
        handshake_timeout: Duration::from_millis(policy.handshake_timeout_ms),
        max_peers: usize::try_from(policy.max_peers).map_err(|_| unavailable())?,
        max_queue_messages: usize::try_from(policy.max_queue_messages)
            .map_err(|_| unavailable())?,
        max_queue_bytes: usize::try_from(policy.max_queue_bytes).map_err(|_| unavailable())?,
        max_streams: usize::try_from(policy.max_streams).map_err(|_| unavailable())?,
    }))
}

fn compile_rtmp_callbacks(
    service: &str,
    application: Option<&str>,
    callbacks: &oxiroute_config::RtmpCallbackConfig,
    outbound_policy: &RtmpOutboundPolicy,
) -> Result<RtmpCallbackPolicy, ServicePlanError> {
    let scope = application.map_or_else(
        || "service".to_owned(),
        |name| format!("application `{name}`"),
    );
    let endpoint = |field: &'static str,
                    value: &Option<String>|
     -> Result<Option<RtmpCallbackEndpoint>, ServicePlanError> {
        value
            .as_deref()
            .map(|value| {
                RtmpCallbackEndpoint::parse(value, outbound_policy).map_err(|_| {
                    ServicePlanError::RtmpCallbackPreflight {
                        service: service.to_owned(),
                        scope: scope.clone(),
                        field,
                    }
                })
            })
            .transpose()
    };
    Ok(RtmpCallbackPolicy {
        on_connect: endpoint("callbacks.on_connect", &callbacks.on_connect)?,
        on_disconnect: endpoint("callbacks.on_disconnect", &callbacks.on_disconnect)?,
        on_publish: endpoint("callbacks.on_publish", &callbacks.on_publish)?,
        on_publish_done: endpoint("callbacks.on_publish_done", &callbacks.on_publish_done)?,
        on_play: endpoint("callbacks.on_play", &callbacks.on_play)?,
        on_play_done: endpoint("callbacks.on_play_done", &callbacks.on_play_done)?,
        on_done: endpoint("callbacks.on_done", &callbacks.on_done)?,
        on_update: endpoint("callbacks.on_update", &callbacks.on_update)?,
        method: match callbacks.notify_method {
            oxiroute_config::RtmpNotifyMethod::Get => RtmpCallbackMethod::Get,
            oxiroute_config::RtmpNotifyMethod::Post => RtmpCallbackMethod::Post,
        },
        timeout: Duration::from_millis(callbacks.timeout_ms),
        update_timeout: Duration::from_millis(callbacks.notify_update_timeout_ms),
        update_strict: callbacks.notify_update_strict,
        relay_redirect: callbacks.notify_relay_redirect,
    })
}

fn compile_rtmp_transport(transport: oxiroute_config::RtmpTransport) -> RtmpTransport {
    match transport {
        oxiroute_config::RtmpTransport::Rtmp => RtmpTransport::Rtmp,
        oxiroute_config::RtmpTransport::Rtmps => RtmpTransport::Rtmps,
    }
}

fn compile_rtmp_client_options(
    tc_url: Option<String>,
    flash_version: Option<String>,
    credentials: Option<&oxiroute_config::RtmpCredentialReference>,
    resolution: impl Fn() -> ServicePlanError,
) -> Result<RtmpClientOptions, ServicePlanError> {
    let credential = credentials
        .map(|reference| {
            let secret = fs::read(&reference.secret_file).map_err(|_| resolution())?;
            if secret.is_empty()
                || secret.len() > 4 * 1_024
                || secret.iter().any(u8::is_ascii_control)
                || std::str::from_utf8(&secret).is_err()
            {
                return Err(resolution());
            }
            Ok(RtmpCredential::new(reference.username.clone(), secret))
        })
        .transpose()?;
    Ok(RtmpClientOptions {
        flash_version: flash_version.unwrap_or_else(|| RtmpClientOptions::default().flash_version),
        playback_buffer_ms: 2_000,
        tc_url,
        credential,
    })
}

fn compile_rtmp_vod(
    service: &str,
    application: &oxiroute_config::RtmpApplication,
    outbound_policy: &RtmpOutboundPolicy,
) -> Result<Option<Arc<VodApplication>>, ServicePlanError> {
    let Some(policy) = &application.vod else {
        return Ok(None);
    };
    let sources = policy
        .sources
        .iter()
        .map(|source| match source {
            oxiroute_config::RtmpVodSource::Local {
                name,
                root_directory,
            } => VodSourceDefinition::Local {
                name: name.clone(),
                root_directory: root_directory.clone(),
            },
            oxiroute_config::RtmpVodSource::Http { name, origin } => VodSourceDefinition::Http {
                name: name.clone(),
                origin: origin.clone(),
            },
        })
        .collect::<Vec<_>>();
    let limits = VodLimits {
        max_sessions: usize::try_from(policy.max_sessions).map_err(|_| {
            ServicePlanError::RuntimePolicyUnavailable {
                policy: "rtmp_services[].applications[].vod.max_sessions",
            }
        })?,
        max_file_bytes: policy.max_file_bytes,
        max_duration: Duration::from_millis(policy.max_duration_ms),
    };
    VodApplication::new(service, &application.name, limits, sources, outbound_policy)
        .map(Arc::new)
        .map(Some)
        .map_err(|_| ServicePlanError::RtmpVodPreflight {
            service: service.to_owned(),
            application: application.name.clone(),
            source_name: policy.sources.first().map_or_else(
                || "unknown".into(),
                |source| match source {
                    oxiroute_config::RtmpVodSource::Local { name, .. }
                    | oxiroute_config::RtmpVodSource::Http { name, .. } => name.clone(),
                },
            ),
        })
}

fn compile_rtmp_hls(
    service: &str,
    application: &oxiroute_config::RtmpApplication,
    stores: &mut HashMap<PathBuf, Arc<MediaStore>>,
) -> Result<Option<Arc<MediaApplication>>, ServicePlanError> {
    let Some(policy) = &application.hls else {
        return Ok(None);
    };
    let invalid = || ServicePlanError::HlsPreflight {
        service: service.to_owned(),
        application: application.name.clone(),
    };
    let limits = MediaStoreLimits {
        max_bytes: policy.max_storage_bytes,
        max_files: usize::try_from(policy.max_storage_files).map_err(|_| invalid())?,
        max_active_streams: usize::try_from(policy.max_active_streams).map_err(|_| invalid())?,
        max_file_bytes: usize::try_from(policy.max_segment_bytes).map_err(|_| invalid())?,
    };
    let store = if let Some(store) = stores.get(&policy.root_directory) {
        Arc::clone(store)
    } else {
        let store = Arc::new(MediaStore::open(&policy.root_directory, limits).map_err(|_| invalid())?);
        stores.insert(policy.root_directory.clone(), Arc::clone(&store));
        store
    };
    let variants = policy
        .variants
        .iter()
        .map(|variant| HlsVariant {
            name: variant.name.clone(),
            bandwidth: variant.bandwidth,
            codecs: variant.codecs.clone(),
            width: variant.width,
            height: variant.height,
        })
        .collect();
    let keys = policy.keys.as_ref().map(|keys| HlsKeyConfig {
        rotation_segments: usize::try_from(keys.rotation_segments).expect("validated HLS key rotation fits usize"),
        url_prefix: keys.url_prefix.clone(),
    });
    let config = HlsOutputConfig {
        store,
        segment_duration: Duration::from_millis(policy.segment_duration_ms),
        max_segment_duration: Duration::from_millis(policy.max_segment_duration_ms),
        playlist_length: Duration::from_millis(policy.playlist_length_ms),
        naming: match policy.fragment_naming {
            oxiroute_config::RtmpHlsFragmentNaming::Sequential => HlsFragmentNaming::Sequential,
            oxiroute_config::RtmpHlsFragmentNaming::Timestamp => HlsFragmentNaming::Timestamp,
            oxiroute_config::RtmpHlsFragmentNaming::System => HlsFragmentNaming::System,
        },
        nested: policy.nested,
        cleanup: policy.cleanup,
        variants,
        keys,
        max_segment_bytes: usize::try_from(policy.max_segment_bytes).map_err(|_| invalid())?,
        max_queue_messages: usize::try_from(policy.max_queue_messages).map_err(|_| invalid())?,
    };
    Ok(Some(Arc::new(MediaApplication::new(Some(Arc::new(config))))))
}

fn compile_rtmp_dash(
    service: &str,
    application: &oxiroute_config::RtmpApplication,
    stores: &mut HashMap<PathBuf, Arc<MediaStore>>,
) -> Result<Option<Arc<DashOutputConfig>>, ServicePlanError> {
    let Some(policy) = &application.dash else {
        return Ok(None);
    };
    let invalid = || ServicePlanError::DashPreflight {
        service: service.to_owned(),
        application: application.name.clone(),
    };
    let limits = MediaStoreLimits {
        max_bytes: policy.max_storage_bytes,
        max_files: usize::try_from(policy.max_storage_files).map_err(|_| invalid())?,
        max_active_streams: usize::try_from(policy.max_active_streams).map_err(|_| invalid())?,
        max_file_bytes: usize::try_from(policy.max_segment_bytes).map_err(|_| invalid())?,
    };
    let store = if let Some(store) = stores.get(&policy.root_directory) {
        Arc::clone(store)
    } else {
        let store = Arc::new(
            MediaStore::open(&policy.root_directory, limits).map_err(|_| invalid())?,
        );
        stores.insert(policy.root_directory.clone(), Arc::clone(&store));
        store
    };
    let config = DashOutputConfig {
        store,
        segment_duration: Duration::from_millis(policy.segment_duration_ms),
        max_segment_duration: Duration::from_millis(policy.max_segment_duration_ms),
        playlist_length: Duration::from_millis(policy.playlist_length_ms),
        naming: match policy.segment_naming {
            oxiroute_config::RtmpDashSegmentNaming::Sequential => DashSegmentNaming::Sequential,
            oxiroute_config::RtmpDashSegmentNaming::Timestamp => DashSegmentNaming::Timestamp,
            oxiroute_config::RtmpDashSegmentNaming::System => DashSegmentNaming::System,
        },
        nested: policy.nested,
        cleanup: policy.cleanup,
        max_segment_bytes: usize::try_from(policy.max_segment_bytes).map_err(|_| invalid())?,
        max_queue_messages: usize::try_from(policy.max_queue_messages).map_err(|_| invalid())?,
    };
    Ok(Some(Arc::new(config)))
}

fn compile_rtmp_exec_profile(
    service: &str,
    application: &oxiroute_config::RtmpApplication,
    profile: &oxiroute_config::RtmpExecProfile,
) -> Result<ExecProfile, ServicePlanError> {
    let invalid = || ServicePlanError::InvalidExecProfile {
        service: service.to_owned(),
        application: application.name.clone(),
        profile: profile.name.clone(),
    };
    let environment = profile
        .environment
        .iter()
        .map(|entry| ExecEnvironment::new(entry.name.clone(), entry.value.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    let limits = ExecLimits::new(
        usize::try_from(profile.max_queue_messages).map_err(|_| invalid())?,
        usize::try_from(profile.max_queue_bytes).map_err(|_| invalid())?,
        usize::try_from(profile.max_stdout_bytes).map_err(|_| invalid())?,
        usize::try_from(profile.max_stderr_bytes).map_err(|_| invalid())?,
        Duration::from_millis(profile.timeout_ms),
        Duration::from_millis(profile.shutdown_timeout_ms),
        usize::try_from(profile.max_processes).map_err(|_| invalid())?,
        Duration::from_millis(profile.respawn_delay_ms),
        usize::try_from(profile.max_respawns).map_err(|_| invalid())?,
    )
    .map_err(|_| invalid())?;
    ExecProfile::new(
        profile.name.clone(),
        profile.application.clone(),
        match profile.mode {
            ConfigExecMode::Command => ExecMode::Command,
            ConfigExecMode::Transcode => ExecMode::Transcode,
        },
        match profile.trigger {
            ConfigExecTrigger::Publisher => ExecTrigger::Publisher,
            ConfigExecTrigger::PublishDone => ExecTrigger::PublishDone,
        },
        profile.executable.clone(),
        profile.arguments.clone(),
        environment,
        profile.working_directory.clone(),
        match profile.filesystem {
            ConfigExecFilesystemPolicy::WorkingDirectory => ExecFilesystemPolicy::WorkingDirectory,
            ConfigExecFilesystemPolicy::Host => return Err(invalid()),
        },
        match profile.network {
            ConfigExecNetworkPolicy::Disabled => ExecNetworkPolicy::Disabled,
            ConfigExecNetworkPolicy::Inherited => ExecNetworkPolicy::Inherited,
        },
        limits,
        profile.respawn,
    )
    .map_err(|_| invalid())
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
        record_mask: RecorderMediaMask::new(
            recorder.record_mask.audio,
            recorder.record_mask.video,
            recorder.record_mask.keyframes,
        ),
        append: recorder.append,
        lock: recorder.lock,
        max_size: recorder.max_size,
        max_frames: recorder.max_frames,
        notify: recorder.notify,
    };
    let store_limits = RecordingStoreLimits {
        max_bytes: recorder.max_storage_bytes,
        max_files: recorder
            .max_storage_files
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid())?,
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

#[allow(clippy::too_many_lines)]
fn compile_listener(
    listener: &oxiroute_config::Listener,
    http_services: &HashMap<String, Arc<HttpServicePlan>>,
    forward_services: &HashMap<String, Arc<ForwardHttp1ServicePlan>>,
    rtmp_services: &HashMap<String, Arc<RtmpServicePlan>>,
    l4_services: &HashMap<String, Arc<L4ServicePlan>>,
    tls_profiles: &crate::tls::TlsProfilePlanMap,
) -> Result<ServiceSpec, ServicePlanError> {
    let tls = match (listener.protocol, listener.tls_profile.as_deref()) {
        (
            Protocol::Http
            | Protocol::ForwardHttp1
            | Protocol::ForwardHttp2
            | Protocol::ForwardHttp3
            | Protocol::Http3,
            Some(profile),
        ) => Some(Arc::clone(tls_profiles.get(profile).ok_or_else(|| {
            ServicePlanError::UnknownListenerTlsProfile {
                listener: listener.name.clone(),
                profile: profile.into(),
            }
        })?)),
        (
            Protocol::Http
            | Protocol::ForwardHttp1
            | Protocol::ForwardHttp2
            | Protocol::ForwardHttp3
            | Protocol::Http3
            | Protocol::Tcp
            | Protocol::Udp
            | Protocol::Rtmp,
            None,
        ) => None,
        (protocol @ (Protocol::Tcp | Protocol::Udp | Protocol::Rtmp), Some(profile)) => {
            return Err(ServicePlanError::UnexpectedListenerTlsProfile {
                listener: listener.name.clone(),
                protocol,
                profile: profile.into(),
            });
        }
    };
    let kind = match (listener.protocol, listener.service.as_deref()) {
        (
            Protocol::Http
            | Protocol::ForwardHttp1
            | Protocol::ForwardHttp2
            | Protocol::ForwardHttp3
            | Protocol::Http3
            | Protocol::Rtmp
            | Protocol::Tcp
            | Protocol::Udp,
            None,
        ) => {
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
        (Protocol::Http3, Some(service)) => {
            ServiceKind::Http3(Arc::clone(http_services.get(service).ok_or_else(|| {
                ServicePlanError::UnknownHttpService {
                    listener: listener.name.clone(),
                    service: service.into(),
                }
            })?))
        }
        (Protocol::ForwardHttp1, Some(service)) => {
            ServiceKind::ForwardHttp1(Arc::clone(forward_services.get(service).ok_or_else(
                || ServicePlanError::UnknownForwardProxyService {
                    listener: listener.name.clone(),
                    service: service.into(),
                },
            )?))
        }
        (Protocol::ForwardHttp2, Some(service)) => {
            ServiceKind::ForwardHttp2(Arc::clone(forward_services.get(service).ok_or_else(
                || ServicePlanError::UnknownForwardProxyService {
                    listener: listener.name.clone(),
                    service: service.into(),
                },
            )?))
        }
        (Protocol::Tcp, Some(service)) => {
            ServiceKind::Tcp(Arc::clone(l4_services.get(service).ok_or_else(|| {
                ServicePlanError::UnknownL4Service {
                    listener: listener.name.clone(),
                    service: service.into(),
                }
            })?))
        }
        (Protocol::ForwardHttp3, Some(service)) => {
            ServiceKind::ForwardHttp3(Arc::clone(forward_services.get(service).ok_or_else(
                || ServicePlanError::UnknownForwardProxyService {
                    listener: listener.name.clone(),
                    service: service.into(),
                },
            )?))
        }
        (Protocol::Udp, Some(service)) => {
            ServiceKind::Udp(Arc::clone(l4_services.get(service).ok_or_else(|| {
                ServicePlanError::UnknownUdpService {
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
    };
    Ok(ServiceSpec {
        name: listener.name.clone(),
        bind: listener.bind.clone(),
        max_connections: listener.max_connections,
        downstream_timeouts: listener.downstream_timeouts,
        proxy_protocol: listener.proxy_protocol,
        kind,
        tls,
    })
}
