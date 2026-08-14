use std::{
    collections::HashMap,
    net::ToSocketAddrs,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub use crate::planning_errors::ServicePlanError;
use crate::{
    ForwardHttp1ServicePlan, ForwardHttp2ServicePlan, HealthSupervisor, L4ServicePlan,
    PassiveFailurePolicy, PoolError, PreparedTls, RelayPolicy, RoundRobinPool, RouteTable,
    RuntimeEndpoint, TlsProfilePlan, TopologySnapshot, health,
    http_action::{
        AccessLog, HttpActionPlan, HttpGzipPlan, HttpRoutePlan, ProxyActionPlan, ProxyPolicyPlan,
        RouteAccess, StaticFilesPlan,
    },
    http_cache::{CachePurgeAccess, DiskBackend, HttpCacheBackend, HttpCachePlan},
    routing::RuntimeServer,
    upstream_peer::UpstreamPlan,
};
use crate::{
    generation_compiler::GenerationCompiler,
    planning_types::{
        CachePolicyBlueprint, CacheStoreBlueprint, HttpActionBlueprint, HttpRouteBlueprint,
        HttpServiceBlueprint, L4ServiceBlueprint, ListenerBlueprint, PoolBlueprint, RtmpSpec,
        ServiceReference,
    },
};
use http::{Method, Uri, uri::Authority};
use oxiroute_cache::{Cache, DiskCache, DiskCacheConfig};
use oxiroute_config::{CachePurgeAuthorization, ListenerBind, Protocol, ValidatedConfig};
use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpCapabilities, RtmpPrepareContext, RtmpPrepareMode,
};

enum DiskBackendRegistryEntry {
    Opening {
        insertion: u64,
        config: DiskCacheConfig,
    },
    Ready {
        insertion: u64,
        config: DiskCacheConfig,
        backend: Weak<DiskBackend>,
    },
}

struct DiskBackendRegistry {
    entries: Mutex<HashMap<PathBuf, DiskBackendRegistryEntry>>,
    changed: Condvar,
}

static DISK_BACKEND_REGISTRY: OnceLock<DiskBackendRegistry> = OnceLock::new();
static NEXT_DISK_BACKEND_INSERTION: AtomicU64 = AtomicU64::new(1);

pub(crate) struct DiskBackendRegistryLease {
    root: PathBuf,
    insertion: u64,
}

#[derive(Debug)]
pub(crate) enum RuntimeAcquisitionError {
    ServicePlan(ServicePlanError),
    PreparationTimedOut,
}

impl From<ServicePlanError> for RuntimeAcquisitionError {
    fn from(error: ServicePlanError) -> Self {
        Self::ServicePlan(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiskBackendOpenError {
    Unavailable,
    TimedOut,
}

impl Drop for DiskBackendRegistryLease {
    fn drop(&mut self) {
        let Some(registry) = DISK_BACKEND_REGISTRY.get() else {
            return;
        };
        let mut entries = registry
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.get(&self.root).is_some_and(|entry| match entry {
            DiskBackendRegistryEntry::Opening { insertion, .. }
            | DiskBackendRegistryEntry::Ready { insertion, .. } => *insertion == self.insertion,
        }) {
            entries.remove(&self.root);
            registry.changed.notify_all();
        }
    }
}

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
    value_plan: oxiroute_rtmp::RtmpServicePlan,
    access_log: Option<Arc<AccessLog>>,
}

impl std::fmt::Debug for RtmpServicePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtmpServicePlan")
            .field("service_id", &self.service_id())
            .finish_non_exhaustive()
    }
}

impl RtmpServicePlan {
    #[must_use]
    pub fn service_id(&self) -> &str {
        self.value_plan.service_id()
    }

    #[must_use]
    pub fn value_plan(&self) -> oxiroute_rtmp::RtmpServicePlan {
        self.value_plan.clone()
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

    /// Writes one fixed-field, redacted RTMP access event without blocking the session task.
    ///
    /// # Errors
    ///
    /// Returns the nonblocking RTMP access-log queue error when it is full or stopped.
    pub fn write_rtmp_access_event(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.access_log
            .as_ref()
            .map_or(Ok(()), |access_log| access_log.write_rtmp(event))
    }
}

#[cfg(test)]
std::thread_local! {
    static RTMP_PREPARES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RTMP_STARTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RTMP_START_EVENTS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static RTMP_START_FAILURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn reset_rtmp_stage_counts() {
    RTMP_PREPARES.set(0);
    RTMP_STARTS.set(0);
    RTMP_START_EVENTS.with(|events| events.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn rtmp_stage_counts() -> (usize, usize) {
    (RTMP_PREPARES.get(), RTMP_STARTS.get())
}

#[cfg(test)]
pub(crate) fn trace_staged_rtmp_prepare() -> RtmpRuntimeFault {
    RTMP_PREPARES.set(RTMP_PREPARES.get() + 1);
    RTMP_RUNTIME_FAULT.get()
}

#[cfg(test)]
pub(crate) fn trace_staged_rtmp_start(service: &str) -> bool {
    RTMP_STARTS.set(RTMP_STARTS.get() + 1);
    RTMP_START_EVENTS.with(|events| events.borrow_mut().push(format!("start:{service}")));
    RTMP_START_FAILURE.with(|failure| failure.borrow().as_deref() == Some(service))
}

#[cfg(test)]
pub(crate) fn trace_rtmp_rollback(service: &str) {
    RTMP_START_EVENTS.with(|events| events.borrow_mut().push(format!("rollback:{service}")));
}

#[cfg(test)]
pub(crate) fn with_rtmp_start_failure<T>(service: &str, run: impl FnOnce() -> T) -> T {
    RTMP_START_FAILURE.with(|failure| failure.replace(Some(service.to_owned())));
    let result = run();
    RTMP_START_FAILURE.with(|failure| failure.replace(None));
    result
}

#[cfg(test)]
pub(crate) fn rtmp_start_events() -> Vec<String> {
    RTMP_START_EVENTS.with(|events| events.borrow().clone())
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RtmpRuntimeFault {
    #[default]
    None,
    RecorderStore,
    AutoPush,
}

#[cfg(test)]
std::thread_local! {
    static RTMP_RUNTIME_FAULT: std::cell::Cell<RtmpRuntimeFault> = const {
        std::cell::Cell::new(RtmpRuntimeFault::None)
    };
}

#[cfg(test)]
pub(crate) fn with_rtmp_runtime_fault<T>(fault: RtmpRuntimeFault, run: impl FnOnce() -> T) -> T {
    RTMP_RUNTIME_FAULT.replace(fault);
    let result = run();
    RTMP_RUNTIME_FAULT.set(RtmpRuntimeFault::None);
    result
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
    pub fn upstream_pools(&self) -> impl Iterator<Item = &Arc<RoundRobinPool>> {
        self.route_plans.values().filter_map(|route| {
            let HttpActionPlan::Proxy(proxy) = &route.action else {
                return None;
            };
            Some(proxy.pool.selector())
        })
    }

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

/// Compiles validated listener definitions into runtime service specifications.
///
/// # Errors
///
/// Returns an error when a programmatically constructed configuration contains invalid routes,
/// pools, service references, or listener/service protocol relationships.
pub fn service_specs(config: &ValidatedConfig) -> Result<Vec<ServiceSpec>, ServicePlanError> {
    if config
        .as_draft()
        .upstream_pools
        .iter()
        .any(|pool| pool.health_check.is_some())
    {
        return Err(ServicePlanError::HealthSupervisorRequired);
    }
    let plan = runtime_plan(config)?;
    let mut acquired = acquire_runtime_services(&plan)?;
    prepare_rtmp_service_specs(acquired.services(), RtmpPrepareMode::Activation)?;
    Ok(acquired.commit().0)
}

/// Immutable, resource-free decisions for one validated runtime generation.
pub struct RuntimePlan {
    pub max_connections: Option<u64>,
    pub rtmp_capabilities: RtmpCapabilities,
    pub rtmp_recording_supported: bool,
    pub topology: Arc<TopologySnapshot>,
    source: Arc<ValidatedConfig>,
    passive_policy: Option<PassiveFailurePolicy>,
}

pub(crate) struct GenerationAcquisition {
    l4_services: Option<Vec<Arc<L4ServicePlan>>>,
    forward_services: Option<Vec<Arc<ForwardHttp1ServicePlan>>>,
    http_services: Option<Vec<Arc<HttpServicePlan>>>,
    compiled_pools: Option<CompiledPools>,
    services: Option<Vec<ServiceSpec>>,
    health_supervisor: Option<HealthSupervisor>,
    pools: Option<Vec<Arc<RoundRobinPool>>>,
    tls: Option<PreparedTls>,
}

impl GenerationAcquisition {
    fn empty() -> Self {
        Self {
            l4_services: None,
            forward_services: None,
            http_services: None,
            compiled_pools: None,
            services: None,
            health_supervisor: None,
            pools: None,
            tls: None,
        }
    }

    pub(crate) fn services(&self) -> &[ServiceSpec] {
        self.services.as_deref().expect("uncommitted services")
    }

    pub(crate) fn pools(&self) -> &[Arc<RoundRobinPool>] {
        self.pools.as_deref().unwrap_or_else(|| {
            self.compiled_pools
                .as_ref()
                .expect("uncommitted pools")
                .ordered
                .as_slice()
        })
    }

    pub(crate) fn tls(&self) -> &PreparedTls {
        self.tls.as_ref().expect("uncommitted TLS")
    }

    pub(crate) fn commit(
        &mut self,
    ) -> (
        Vec<ServiceSpec>,
        Option<HealthSupervisor>,
        Vec<Arc<RoundRobinPool>>,
        PreparedTls,
    ) {
        self.l4_services.take();
        self.forward_services.take();
        self.http_services.take();
        self.compiled_pools.take();
        (
            self.services.take().expect("uncommitted services"),
            self.health_supervisor.take(),
            self.pools.take().expect("uncommitted pools"),
            self.tls.take().expect("uncommitted TLS"),
        )
    }
}

impl Drop for GenerationAcquisition {
    fn drop(&mut self) {
        self.services.take();
        self.l4_services.take();
        self.forward_services.take();
        self.http_services.take();
        self.health_supervisor.take();
        self.pools.take();
        self.compiled_pools.take();
        self.tls.take();
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum AcquisitionMode {
    Activate,
    Validate,
}

/// Compiles one immutable runtime generation including traffic and health services.
///
/// # Errors
///
/// Returns an error when a pool, route, reference, or health probe cannot be compiled.
///
/// ```compile_fail
/// use oxiroute_config::ConfigDraft;
///
/// let draft: ConfigDraft = todo!();
/// let _ = oxiroute_server::runtime_plan(&draft);
/// ```
pub fn runtime_plan(config: &ValidatedConfig) -> Result<RuntimePlan, ServicePlanError> {
    runtime_plan_with_passive_failure_policy_internal(config, None)
}

/// Validates runtime acquisitions without retaining resources, then returns the immutable plan.
///
/// # Errors
///
/// Returns an error when any runtime resource cannot be acquired safely.
pub fn validate_runtime_plan(config: &ValidatedConfig) -> Result<RuntimePlan, ServicePlanError> {
    let plan = runtime_plan(config)?;
    let acquired = validate_runtime_services(&plan)?;
    prepare_rtmp_service_specs(acquired.services(), RtmpPrepareMode::Validation)?;
    Ok(plan)
}

fn prepare_rtmp_service_specs(
    services: &[ServiceSpec],
    mode: RtmpPrepareMode,
) -> Result<(), ServicePlanError> {
    let mut plans = Vec::new();
    for service in services {
        let ServiceKind::Rtmp(service) = &service.kind else {
            continue;
        };
        if !plans
            .iter()
            .any(|plan: &oxiroute_rtmp::RtmpServicePlan| plan.service_id() == service.service_id())
        {
            plans.push(service.value_plan());
        }
    }
    let listener_addresses = services.iter().filter_map(|service| match service.bind {
        ListenerBind::Socket { address } | ListenerBind::Udp { address } => Some(address),
        ListenerBind::Unix { .. } => None,
    });
    let context = RtmpPrepareContext::new(mode, listener_addresses);
    PreparedRtmpRuntimeSet::prepare(plans, &context, Instant::now() + Duration::from_secs(5))
        .map(drop)
        .map_err(ServicePlanError::RtmpRuntimePreparation)
}

pub(crate) fn validation_plan(config: &ValidatedConfig) -> Result<RuntimePlan, ServicePlanError> {
    runtime_plan_with_passive_failure_policy_internal(config, None)
}

/// Compiles one immutable runtime generation with an explicit passive endpoint policy.
///
/// # Errors
///
/// Returns an error when a pool, route, reference, or health probe cannot be compiled, including
/// when the passive policy exceeds its runtime bounds.
pub fn runtime_plan_with_passive_failure_policy(
    config: &ValidatedConfig,
    passive_policy: PassiveFailurePolicy,
) -> Result<RuntimePlan, ServicePlanError> {
    runtime_plan_with_passive_failure_policy_internal(config, Some(passive_policy))
}

fn runtime_plan_with_passive_failure_policy_internal(
    config: &ValidatedConfig,
    passive_policy: Option<PassiveFailurePolicy>,
) -> Result<RuntimePlan, ServicePlanError> {
    let blueprint = GenerationCompiler::compile(config)?;
    if let Some(passive_policy) = passive_policy {
        passive_policy
            .validate()
            .map_err(|source| ServicePlanError::Pool {
                pool: "passive-policy-override".into(),
                source,
            })?;
    }
    Ok(RuntimePlan {
        max_connections: blueprint.max_connections,
        rtmp_capabilities: blueprint.rtmp_capabilities,
        rtmp_recording_supported: blueprint.rtmp_recording_supported,
        topology: blueprint.topology,
        source: Arc::new(config.clone()),
        passive_policy,
    })
}

pub(crate) fn acquire_runtime_services(
    plan: &RuntimePlan,
) -> Result<GenerationAcquisition, ServicePlanError> {
    acquire_runtime_services_with_deadline(plan, None).map_err(|error| match error {
        RuntimeAcquisitionError::ServicePlan(error) => error,
        RuntimeAcquisitionError::PreparationTimedOut => {
            unreachable!("unbounded runtime acquisition cannot time out")
        }
    })
}

pub(crate) fn acquire_runtime_services_with_deadline(
    plan: &RuntimePlan,
    deadline: Option<Instant>,
) -> Result<GenerationAcquisition, RuntimeAcquisitionError> {
    acquire_runtime_services_with_mode(plan, AcquisitionMode::Activate, deadline)
}

pub(crate) fn validate_runtime_services(
    plan: &RuntimePlan,
) -> Result<GenerationAcquisition, ServicePlanError> {
    acquire_runtime_services_with_mode(plan, AcquisitionMode::Validate, None).map_err(|error| {
        match error {
            RuntimeAcquisitionError::ServicePlan(error) => error,
            RuntimeAcquisitionError::PreparationTimedOut => {
                unreachable!("runtime validation has no deadline")
            }
        }
    })
}

fn acquire_runtime_services_with_mode(
    plan: &RuntimePlan,
    mode: AcquisitionMode,
    deadline: Option<Instant>,
) -> Result<GenerationAcquisition, RuntimeAcquisitionError> {
    let blueprint = GenerationCompiler::compile(&plan.source)?;
    let mut acquired = GenerationAcquisition::empty();
    acquired.tls = Some(
        crate::tls::prepare_tls_blueprint(&blueprint.tls)
            .map_err(|source| ServicePlanError::Tls(Box::new(source)))?,
    );
    let pool_specs = blueprint.pool_specs?;
    acquired.compiled_pools = Some(compile_pools(
        &pool_specs,
        &blueprint.protected_addresses,
        plan.passive_policy,
    )?);
    let cache_specs = blueprint.cache_specs?;
    let http_service_specs = blueprint.http_service_specs?;
    acquired.http_services = Some(compile_http_services(
        &http_service_specs,
        &cache_specs,
        &acquired
            .compiled_pools
            .as_ref()
            .expect("acquired pools")
            .plans,
        mode,
        deadline,
    )?);
    acquired.forward_services = Some(compile_forward_proxy_services(
        &blueprint.forward_service_specs?,
        &cache_specs,
        mode,
        deadline,
    )?);
    let rtmp_services = compile_rtmp_services(&blueprint.rtmp_specs?, mode)?;
    acquired.l4_services = Some(compile_l4_services(
        &blueprint.l4_service_specs?,
        &acquired
            .compiled_pools
            .as_ref()
            .expect("acquired pools")
            .plans,
    ));
    acquired.services = Some(
        blueprint
            .listener_specs?
            .iter()
            .map(|listener| {
                compile_listener(
                    listener,
                    acquired.http_services.as_deref().expect("acquired HTTP"),
                    acquired
                        .forward_services
                        .as_deref()
                        .expect("acquired forward proxy"),
                    &rtmp_services,
                    acquired.l4_services.as_deref().expect("acquired L4"),
                    acquired.tls().profiles(),
                    &blueprint.tls,
                )
            })
            .collect(),
    );
    let pools = acquired.compiled_pools.take().expect("acquired pools");
    acquired.health_supervisor =
        (!pools.health_groups.is_empty()).then(|| HealthSupervisor::new(pools.health_groups));
    acquired.pools = Some(pools.ordered);
    Ok(acquired)
}

fn compile_forward_proxy_services(
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

struct CompiledPools {
    plans: Vec<Arc<UpstreamPlan>>,
    health_groups: Vec<health::HealthGroup>,
    ordered: Vec<Arc<RoundRobinPool>>,
}

#[expect(
    clippy::too_many_lines,
    reason = "pool compilation performs one atomic validation and construction pass"
)]
fn compile_pools(
    pool_specs: &[PoolBlueprint],
    protected_addresses: &Arc<[std::net::SocketAddr]>,
    passive_policy_override: Option<PassiveFailurePolicy>,
) -> Result<CompiledPools, ServicePlanError> {
    let mut pools = HashMap::with_capacity(pool_specs.len());
    let mut health_groups = Vec::new();
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
        let compiled = Arc::new(UpstreamPlan::with_http_policy(
            Arc::clone(&selector),
            tls,
            pool.connect_timeout,
            pool.server_timeout,
            pool.connection_reuse,
            pool.min_http_version,
            h3,
        ));
        if let Some(health_check) = &pool.health {
            health_groups.push(health::acquire_health_group(
                &pool.name,
                &selector,
                health_check,
            ));
        }
        pools.insert(pool.name.clone(), Arc::clone(&compiled));
        plans.push(compiled);
        ordered.push(selector);
    }
    Ok(CompiledPools {
        plans,
        health_groups,
        ordered,
    })
}

fn compile_http_services(
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
            policy,
            cache,
        } => compile_proxy_action(
            service,
            route_index,
            *pool,
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
    let route_id = route.route.route_id().to_owned();
    let plan = Arc::new(HttpRoutePlan {
        access,
        action,
        policy: route.policy,
        route_id,
    });
    Ok(plan)
}

#[cfg(test)]
fn open_shared_disk_backend(
    root: &std::path::Path,
    config: &DiskCacheConfig,
) -> Result<Arc<DiskBackend>, ()> {
    open_shared_disk_backend_with_deadline(root, config, None).map_err(|_| ())
}

#[expect(
    clippy::too_many_lines,
    reason = "the token-CAS loop keeps one registry transition state machine auditable"
)]
fn open_shared_disk_backend_with_deadline(
    root: &std::path::Path,
    config: &DiskCacheConfig,
    deadline: Option<Instant>,
) -> Result<Arc<DiskBackend>, DiskBackendOpenError> {
    let registry = DISK_BACKEND_REGISTRY.get_or_init(|| DiskBackendRegistry {
        entries: Mutex::new(HashMap::new()),
        changed: Condvar::new(),
    });
    loop {
        let snapshot = {
            let mut entries = registry
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                match entries.get(root) {
                    Some(DiskBackendRegistryEntry::Opening {
                        insertion,
                        config: opening_config,
                    }) => {
                        if opening_config != config {
                            return Err(DiskBackendOpenError::Unavailable);
                        }
                        let observed = *insertion;
                        let same_opening = |entries: &mut HashMap<_, _>| {
                            entries.get(root).is_some_and(|entry| {
                                matches!(
                                    entry,
                                    DiskBackendRegistryEntry::Opening { insertion, .. }
                                        if *insertion == observed
                                )
                            })
                        };
                        if let Some(deadline) = deadline {
                            let now = Instant::now();
                            if now >= deadline {
                                return Err(DiskBackendOpenError::TimedOut);
                            }
                            let (next, _) = registry
                                .changed
                                .wait_timeout_while(
                                    entries,
                                    deadline.saturating_duration_since(now),
                                    same_opening,
                                )
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            entries = next;
                            if same_opening(&mut entries) && Instant::now() >= deadline {
                                return Err(DiskBackendOpenError::TimedOut);
                            }
                        } else {
                            entries = registry
                                .changed
                                .wait_while(entries, same_opening)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    }
                    Some(DiskBackendRegistryEntry::Ready {
                        insertion,
                        config: existing_config,
                        backend,
                    }) => {
                        break Some((*insertion, existing_config.clone(), backend.clone()));
                    }
                    None => {
                        let insertion = NEXT_DISK_BACKEND_INSERTION.fetch_add(1, Ordering::Relaxed);
                        entries.insert(
                            root.to_owned(),
                            DiskBackendRegistryEntry::Opening {
                                insertion,
                                config: config.clone(),
                            },
                        );
                        break None;
                    }
                }
            }
        };

        if let Some((insertion, existing_config, backend)) = snapshot {
            #[cfg(test)]
            run_disk_registry_snapshot_hook(root);
            let existing = backend.upgrade();
            if let Some(existing) = existing {
                return (&existing_config == config)
                    .then_some(existing)
                    .ok_or(DiskBackendOpenError::Unavailable);
            }
            let mut entries = registry
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries.get(root).is_some_and(|entry| {
                matches!(
                    entry,
                    DiskBackendRegistryEntry::Ready {
                        insertion: current,
                        ..
                    } if *current == insertion
                )
            }) {
                entries.remove(root);
                registry.changed.notify_all();
            }
            continue;
        }

        let insertion = {
            let entries = registry
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match entries.get(root) {
                Some(DiskBackendRegistryEntry::Opening { insertion, .. }) => *insertion,
                Some(DiskBackendRegistryEntry::Ready { .. }) | None => continue,
            }
        };
        #[cfg(test)]
        run_disk_registry_opening_hook(root);
        let Ok(cache) = DiskCache::open(root, config.clone()) else {
            remove_disk_registry_opening(registry, root, insertion);
            return Err(DiskBackendOpenError::Unavailable);
        };
        let cache = Arc::new(cache);
        let backend = Arc::new(DiskBackend::new(
            cache,
            DiskBackendRegistryLease {
                root: root.to_owned(),
                insertion,
            },
        ));
        let mut entries = registry
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owns_opening = entries.get(root).is_some_and(|entry| {
            matches!(
                entry,
                DiskBackendRegistryEntry::Opening {
                    insertion: current,
                    ..
                } if *current == insertion
            )
        });
        if owns_opening {
            entries.insert(
                root.to_owned(),
                DiskBackendRegistryEntry::Ready {
                    insertion,
                    config: config.clone(),
                    backend: Arc::downgrade(&backend),
                },
            );
            registry.changed.notify_all();
            drop(entries);
            return Ok(backend);
        }
        drop(entries);
        drop(backend);
    }
}

fn remove_disk_registry_opening(
    registry: &DiskBackendRegistry,
    root: &std::path::Path,
    insertion: u64,
) {
    let mut entries = registry
        .entries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if entries.get(root).is_some_and(|entry| {
        matches!(
            entry,
            DiskBackendRegistryEntry::Opening {
                insertion: current,
                ..
            } if *current == insertion
        )
    }) {
        entries.remove(root);
        registry.changed.notify_all();
    }
}

#[cfg(test)]
struct DiskRegistrySnapshotHook {
    root: PathBuf,
    reached: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
static DISK_REGISTRY_SNAPSHOT_HOOK: Mutex<Option<Arc<DiskRegistrySnapshotHook>>> = Mutex::new(None);

#[cfg(test)]
static DISK_REGISTRY_OPENING_HOOKS: OnceLock<
    Mutex<HashMap<PathBuf, Arc<DiskRegistrySnapshotHook>>>,
> = OnceLock::new();

#[cfg(test)]
fn run_disk_registry_snapshot_hook(root: &std::path::Path) {
    let hook = DISK_REGISTRY_SNAPSHOT_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.root == root)
        .cloned();
    if let Some(hook) = hook {
        hook.reached.wait();
        hook.release.wait();
    }
}

#[cfg(test)]
fn run_disk_registry_opening_hook(root: &std::path::Path) {
    let hook = DISK_REGISTRY_OPENING_HOOKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root)
        .cloned();
    if let Some(hook) = hook {
        hook.reached.wait();
        hook.release.wait();
    }
}

#[cfg(test)]
struct DiskRegistrySnapshotHookGuard;

#[cfg(test)]
pub(crate) struct DiskRegistryOpeningHookGuard {
    root: PathBuf,
}

#[cfg(test)]
impl Drop for DiskRegistrySnapshotHookGuard {
    fn drop(&mut self) {
        DISK_REGISTRY_SNAPSHOT_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
impl Drop for DiskRegistryOpeningHookGuard {
    fn drop(&mut self) {
        DISK_REGISTRY_OPENING_HOOKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.root);
    }
}

#[cfg(test)]
fn install_disk_registry_snapshot_hook(
    root: PathBuf,
) -> (
    DiskRegistrySnapshotHookGuard,
    Arc<std::sync::Barrier>,
    Arc<std::sync::Barrier>,
) {
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let hook = Arc::new(DiskRegistrySnapshotHook {
        root,
        reached: Arc::clone(&reached),
        release: Arc::clone(&release),
    });
    let replaced = DISK_REGISTRY_SNAPSHOT_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace(hook);
    assert!(replaced.is_none(), "disk registry hook already installed");
    (DiskRegistrySnapshotHookGuard, reached, release)
}

#[cfg(test)]
pub(crate) fn install_disk_registry_opening_hook(
    root: PathBuf,
) -> (
    DiskRegistryOpeningHookGuard,
    Arc<std::sync::Barrier>,
    Arc<std::sync::Barrier>,
) {
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let hook = Arc::new(DiskRegistrySnapshotHook {
        root: root.clone(),
        reached: Arc::clone(&reached),
        release: Arc::clone(&release),
    });
    let replaced = DISK_REGISTRY_OPENING_HOOKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.clone(), hook);
    assert!(
        replaced.is_none(),
        "disk registry opening hook already installed"
    );
    (DiskRegistryOpeningHookGuard { root }, reached, release)
}

#[cfg(test)]
pub(crate) fn disk_backend_registry_contains(root: &std::path::Path) -> bool {
    DISK_BACKEND_REGISTRY.get().is_some_and(|registry| {
        registry
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(root)
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "proxy compilation carries route errors and shared cache-generation state"
)]
fn compile_proxy_action(
    service: &str,
    route: usize,
    pool_index: usize,
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
        policy: policy.clone_with_cache(cache),
    }))
}

#[allow(clippy::too_many_arguments)]
fn acquire_cache_policy(
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

fn compile_l4_services(
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
    crate::rtmp_value_plan::compile_rtmp_value_plans_from_draft(config.as_draft())
}

fn compile_rtmp_services(
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

fn acquire_access_log(
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

#[allow(clippy::too_many_lines)]
fn compile_listener(
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
        proxy_protocol: listener.proxy_protocol,
        kind,
        tls,
    }
}

#[cfg(test)]
mod disk_registry_tests {
    use super::*;

    static SNAPSHOT_HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn incompatible_open_does_not_hold_registry_lock_while_final_backend_retires() {
        let _test_guard = SNAPSHOT_HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory = tempfile::tempdir().expect("disk registry root");
        let root = directory.path().join("cache");
        let config = DiskCacheConfig::default();
        let backend = open_shared_disk_backend(&root, &config).expect("initial backend");
        let mut incompatible = config.clone();
        incompatible.max_disk_bytes += 1;
        let (_hook, reached, release) = install_disk_registry_snapshot_hook(root.clone());

        let opener_root = root.clone();
        let opener_config = incompatible.clone();
        let opener =
            std::thread::spawn(move || open_shared_disk_backend(&opener_root, &opener_config));
        reached.wait();
        drop(backend);
        release.wait();

        let replacement = opener
            .join()
            .expect("incompatible opener thread")
            .expect("retired entry permits replacement config");
        assert_eq!(replacement.disk_config(), &incompatible);
    }

    #[test]
    fn concurrent_compatible_opens_publish_one_shared_backend() {
        let directory = tempfile::tempdir().expect("disk registry root");
        let root = directory.path().join("cache");
        let start = Arc::new(std::sync::Barrier::new(3));
        let open = |start: Arc<std::sync::Barrier>, root: PathBuf| {
            std::thread::spawn(move || {
                start.wait();
                open_shared_disk_backend(&root, &DiskCacheConfig::default())
                    .expect("compatible backend")
            })
        };
        let first = open(Arc::clone(&start), root.clone());
        let second = open(Arc::clone(&start), root.clone());
        start.wait();

        let first = first.join().expect("first opener");
        let second = second.join().expect("second opener");
        assert!(Arc::ptr_eq(&first, &second));
        drop(first);
        drop(second);
        assert!(!disk_backend_registry_contains(&root));
    }

    #[test]
    fn compatible_waiter_times_out_without_removing_the_opening_insertion() {
        let directory = tempfile::tempdir().expect("disk registry root");
        let root = directory.path().join("cache");
        let config = DiskCacheConfig::default();
        let (_hook, reached, release) = install_disk_registry_opening_hook(root.clone());
        let opener_root = root.clone();
        let opener_config = config.clone();
        let opener = std::thread::spawn(move || {
            open_shared_disk_backend(&opener_root, &opener_config).expect("opening backend")
        });
        reached.wait();

        let deadline = Instant::now() + Duration::from_millis(25);
        let Err(error) = open_shared_disk_backend_with_deadline(&root, &config, Some(deadline))
        else {
            panic!("compatible waiter did not time out")
        };

        assert_eq!(error, DiskBackendOpenError::TimedOut);
        assert!(Instant::now() >= deadline);
        assert!(disk_backend_registry_contains(&root));
        release.wait();
        let backend = opener.join().expect("opener thread");
        drop(backend);
        assert!(!disk_backend_registry_contains(&root));
    }

    #[test]
    fn stale_snapshot_cleanup_does_not_remove_replacement_backend() {
        let _test_guard = SNAPSHOT_HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory = tempfile::tempdir().expect("disk registry root");
        let root = directory.path().join("cache");
        let config = DiskCacheConfig::default();
        let original = open_shared_disk_backend(&root, &config).expect("original backend");
        let (hook, reached, release) = install_disk_registry_snapshot_hook(root.clone());

        let stale_root = root.clone();
        let stale_config = config.clone();
        let stale = std::thread::spawn(move || {
            open_shared_disk_backend(&stale_root, &stale_config).expect("stale opener")
        });
        reached.wait();
        drop(hook);
        drop(original);
        let replacement = open_shared_disk_backend(&root, &config).expect("replacement backend");
        release.wait();

        let observed = stale.join().expect("stale opener thread");
        assert!(Arc::ptr_eq(&observed, &replacement));
        assert!(disk_backend_registry_contains(&root));
        drop(observed);
        drop(replacement);
        assert!(!disk_backend_registry_contains(&root));
    }
}

#[cfg(test)]
mod rtmp_value_plan_tests {
    use std::error::Error as _;

    use oxiroute_config::ConfigDraft;

    use crate::{
        planning_errors::rtmp_preparation_error,
        rtmp_value_plan::compile_rtmp_value_plans_from_draft,
    };

    use super::*;

    #[test]
    fn validated_config_translates_to_value_plans_without_acquisition() {
        let config = oxiroute_config_source::load_lua(
            r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "streaming",
      callbacks = { on_connect = "https://callback.example.test/connect" },
      auto_push = {
        enabled = true,
        socket_dir = "/not-created/auto-push",
        secret_file = "/not-read/auto-push-secret",
      },
      exec_profiles = {
        {
          name = "publisher",
          application = "live",
          executable = "/not-run/transcoder",
          arguments = { "--input", "$name" },
          environment = { { name = "TOKEN", value = "not-exposed" } },
          working_directory = "/not-read/work",
        },
      },
      applications = {
        {
          name = "live",
          live = true,
          publish = {
            rules = { { action = "allow", network = "2001:db8::/128" } },
            token = { source = "stream_query", parameter = "token", secret = "secret" },
          },
          push_targets = {
            {
              host = "unresolved.example.test",
              application = "$name",
              credentials = {
                username = "relay",
                secret_file = "/not-read/relay-secret",
              },
            },
          },
          callbacks = { on_publish = "https://callback.example.test/publish" },
          vod = {
            sources = {
              { type = "local", name = "archive", root_directory = "/not-read/vod" },
              { type = "http", name = "origin", origin = "https://media.example.test/library" },
            },
          },
          hls = { root_directory = "/not-created/hls" },
          dash = { root_directory = "/not-created/dash" },
          recorders = {
            { name = "archive", root_directory = "/not-created/recordings" },
          },
        },
      },
    },
  },
}
"#,
        )
        .expect("representative validated RTMP config");

        let plans = compile_rtmp_value_plans(&config).expect("opaque RTMP value plans");

        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        assert_eq!(plan.service_id(), "streaming");
        assert!(plan.auto_push().is_some());
        let application = &plan.applications()[0];
        assert!(application.media().is_some());
        assert!(application.vod().is_some());
        assert_eq!(application.recorders().len(), 1);
        assert_eq!(application.exec().len(), 1);
        assert_eq!(application.relay().push().len(), 1);
    }

    #[test]
    fn canonical_minimum_and_maximum_bounds_translate_to_value_plans() {
        let minimum = oxiroute_config_source::load_lua(
            r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "minimum",
      outbound_chunk_size = 1,
      max_inbound_message_size = 1,
      ack_window_size = 1,
      applications = {
        {
          name = "live",
          limits = { max_connections = 1, max_publishers = 1, max_viewers = 1 },
          fanout = {
            max_subscribers = 1,
            max_queue_messages_per_subscriber = 1,
            max_queue_bytes_per_subscriber = 1,
          },
          relay = {
            max_queue_messages = 1,
            max_queue_bytes = 1,
            buffer_ms = 1,
            push_reconnect_ms = 1,
            pull_reconnect_ms = 1,
            dns_refresh_ms = 1000,
            connect_timeout_ms = 1,
            handshake_timeout_ms = 1,
          },
        },
      },
    },
  },
}
"#,
        )
        .expect("canonical RTMP minima");
        assert_eq!(compile_rtmp_value_plans(&minimum).unwrap().len(), 1);

        let draft = minimum.to_draft();
        let mut service = draft.rtmp_services[0].clone();
        service.name = "maximum".into();
        service.outbound_chunk_size = 1_048_576;
        service.max_inbound_message_size = 8_388_608;
        service.ack_window_size = u32::MAX;
        let application = &mut service.applications[0];
        application.limits.max_connections = 100_000;
        application.limits.max_publishers = 10_000;
        application.limits.max_viewers = 1_000_000;
        application.fanout.max_subscribers = 1_000_000;
        application.fanout.max_queue_messages_per_subscriber = 65_536;
        application.fanout.max_queue_bytes_per_subscriber = 1_073_741_824;
        application.relay.max_queue_messages = 65_536;
        application.relay.max_queue_bytes = 1_073_741_824;
        application.relay.buffer_ms = 60_000;
        application.relay.push_reconnect_ms = 300_000;
        application.relay.pull_reconnect_ms = 300_000;
        application.relay.dns_refresh_ms = 300_000;
        application.relay.connect_timeout_ms = 30_000;
        application.relay.handshake_timeout_ms = 30_000;
        let maximum = ConfigDraft {
            rtmp_services: vec![service],
            ..draft
        }
        .validate()
        .expect("canonical RTMP maxima");

        let plans = compile_rtmp_value_plans(&maximum).expect("maximum value plans");
        assert_eq!(plans[0].outbound_chunk_size(), 1_048_576);
        assert_eq!(
            plans[0].applications()[0].fanout().max_subscribers(),
            1_000_000
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn covered_canonical_policy_matrix_rejects_before_value_translation() {
        let base = oxiroute_config_source::load_lua(
            r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "streaming",
      applications = { { name = "live", live = true } },
    },
  },
}
"#,
        )
        .expect("canonical policy base")
        .to_draft();

        let mut invalid = Vec::new();

        let mut duplicate_services = base.clone();
        duplicate_services
            .rtmp_services
            .push(duplicate_services.rtmp_services[0].clone());
        invalid.push(duplicate_services);

        let mut duplicate_applications = base.clone();
        let application = duplicate_applications.rtmp_services[0].applications[0].clone();
        duplicate_applications.rtmp_services[0]
            .applications
            .push(application);
        invalid.push(duplicate_applications);

        let mut duplicate_push = base.clone();
        duplicate_push.rtmp_services[0].outbound_policy.deny_private = false;
        let target = oxiroute_config::RtmpPushTarget {
            host: "origin.example.test".into(),
            port: 1935,
            application: "$name".into(),
            scheme: oxiroute_config::RtmpTransport::Rtmp,
            stream_name: None,
            tc_url: None,
            flash_version: None,
            credentials: None,
        };
        duplicate_push.rtmp_services[0].applications[0].push_targets = vec![target.clone(), target];
        invalid.push(duplicate_push);

        let mut non_live_push = base.clone();
        non_live_push.rtmp_services[0].applications[0].live = false;
        non_live_push.rtmp_services[0].applications[0]
            .push_targets
            .push(oxiroute_config::RtmpPushTarget {
                host: "origin.example.test".into(),
                port: 1935,
                application: "$name".into(),
                scheme: oxiroute_config::RtmpTransport::Rtmp,
                stream_name: None,
                tc_url: None,
                flash_version: None,
                credentials: None,
            });
        invalid.push(non_live_push);

        let mut non_live_media = base.clone();
        non_live_media.rtmp_services[0].applications[0].live = false;
        non_live_media.rtmp_services[0].applications[0].hls =
            Some(serde_json::from_value(serde_json::json!({"root_directory": "/media"})).unwrap());
        invalid.push(non_live_media);

        let mut shared_root = base.clone();
        let first: oxiroute_config::RtmpHlsPolicy = serde_json::from_value(serde_json::json!({
            "root_directory": "/shared"
        }))
        .unwrap();
        let mut second = first.clone();
        second.max_storage_bytes += 1;
        shared_root.rtmp_services[0].applications[0].hls = Some(first);
        let mut second_application = shared_root.rtmp_services[0].applications[0].clone();
        second_application.name = "backup".into();
        second_application.hls = Some(second);
        shared_root.rtmp_services[0]
            .applications
            .push(second_application);
        invalid.push(shared_root);

        let mut unknown_exec_application = base.clone();
        unknown_exec_application.rtmp_services[0]
            .exec_profiles
            .push(
                serde_json::from_value(serde_json::json!({
                    "name": "profile",
                    "application": "missing",
                    "executable": "/bin/transcoder",
                    "working_directory": "/work"
                }))
                .unwrap(),
            );
        invalid.push(unknown_exec_application);

        for (index, draft) in invalid.into_iter().enumerate() {
            assert!(draft.validate().is_err(), "canonical policy case {index}");
        }

        let mut too_many_services = base;
        for index in 1..=64 {
            let mut service = too_many_services.rtmp_services[0].clone();
            service.name = format!("service-{index}");
            too_many_services.rtmp_services.push(service);
        }
        assert!(
            too_many_services.validate().is_err(),
            "canonical service count +1"
        );
    }

    #[test]
    fn translator_errors_map_to_typed_redacted_service_plan_errors() {
        const SECRET_PATH: &str = "/secret/tenant/transcoder-token";
        const SECRET_TOKEN: &str = "token=super-secret";
        const SECRET_URL: &str = "https://user:password@example.test/private";
        let mut config = oxiroute_config_source::load_lua(
            r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "streaming",
      exec_profiles = {
        {
          name = "publisher",
          application = "live",
          executable = "/bin/transcoder",
          working_directory = "/work",
        },
      },
      applications = { { name = "live", live = true } },
    },
  },
}
"#,
        )
        .unwrap()
        .to_draft();
        config.rtmp_services[0].exec_profiles[0].executable = "/bin/sh".into();
        config.rtmp_services[0].exec_profiles[0].working_directory = SECRET_PATH.into();
        config.rtmp_services[0].exec_profiles[0].arguments = vec![SECRET_TOKEN.into()];
        config.rtmp_services[0].applications[0].callbacks.on_publish = Some(SECRET_URL.into());
        config.rtmp_services[0].applications[0].recorders.push(
            serde_json::from_value(serde_json::json!({
                "name": "archive",
                "root_directory": SECRET_PATH,
            }))
            .unwrap(),
        );

        let source = compile_rtmp_value_plans_from_draft(&config).unwrap_err();
        let error = rtmp_preparation_error(source);
        let ServicePlanError::RtmpPreparation(source) = &error else {
            panic!("typed RTMP preparation error")
        };
        assert_eq!(source.service_id(), Some("streaming"));
        assert_eq!(source.application_name(), Some("live"));
        assert_eq!(source.profile_name(), Some("publisher"));
        assert_eq!(source.recorder_name(), None);
        assert_eq!(source.field(), "exec.executable");
        assert_eq!(source.category(), oxiroute_rtmp::RtmpPrepareCategory::Value);
        assert!(error.source().is_some());
        assert!(error.source().unwrap().source().is_some());

        let rendered = format!("{error:#}");
        for expected in [
            "service `streaming`",
            "application `live`",
            "profile `publisher`",
            "exec.executable",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected}: {rendered}"
            );
        }
        for secret in [SECRET_PATH, SECRET_TOKEN, SECRET_URL] {
            assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
            let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            while let Some(source) = current {
                assert!(
                    !source.to_string().contains(secret),
                    "source leaked {secret}"
                );
                current = source.source();
            }
        }

        config.rtmp_services[0].exec_profiles.clear();
        config.rtmp_services[0].applications[0].callbacks.on_publish = None;
        config.rtmp_services[0].applications[0].recorders[0].root_directory =
            "relative-secret".into();
        let recorder = compile_rtmp_value_plans_from_draft(&config).unwrap_err();
        assert_eq!(recorder.service_id(), Some("streaming"));
        assert_eq!(recorder.application_name(), Some("live"));
        assert_eq!(recorder.recorder_name(), Some("archive"));
        assert_eq!(recorder.profile_name(), None);
        assert_eq!(recorder.field(), "recorder.root_directory");
        assert!(!recorder.to_string().contains("relative-secret"));
    }
}
