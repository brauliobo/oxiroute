#[allow(clippy::wildcard_imports)]
use super::*;
use crate::http::proxy::lane::LaneRoute;

#[derive(Clone, Debug)]
pub struct ServiceSpec {
    pub name: String,
    pub bind: ListenerBind,
    pub max_connections: Option<u64>,
    pub downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy,
    pub h2_windows: oxiroute_config::H2Windows,
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
    pub(super) value_plan: oxiroute_rtmp::RtmpServicePlan,
    pub(super) access_log: Option<Arc<AccessLog>>,
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
    /// The plans in the order of the routes of the table.
    route_plans: Box<[Arc<HttpRoutePlan>]>,
    upstream_io_timeout: Duration,
    routes: RouteTable,
}

impl HttpServicePlan {
    pub fn upstream_pools(&self) -> impl Iterator<Item = &Arc<RoundRobinPool>> {
        self.route_plans.iter().filter_map(|route| {
            let HttpActionPlan::Proxy(proxy) = &route.action else {
                return None;
            };
            Some(proxy.pool.selector())
        })
    }

    #[cfg(test)]
    pub(crate) fn new(
        max_request_body_bytes: Option<u64>,
        route_plans: Vec<Arc<HttpRoutePlan>>,
        upstream_io_timeout: Duration,
        routes: RouteTable,
    ) -> Self {
        Self {
            access_log: None,
            automatic_response_headers: true,
            gzip: None,
            max_request_body_bytes,
            route_plans: route_plans.into(),
            upstream_io_timeout,
            routes,
        }
    }

    pub(super) fn with_http_policy(
        automatic_response_headers: bool,
        max_request_body_bytes: Option<u64>,
        route_plans: Vec<Arc<HttpRoutePlan>>,
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
            route_plans: route_plans.into(),
            upstream_io_timeout,
            routes,
        }
    }

    pub(crate) fn select_route(
        &self,
        authority: Option<&Authority>,
        uri: &Uri,
        method: &Method,
    ) -> Option<&Arc<HttpRoutePlan>> {
        let authority = authority.or_else(|| uri.authority());
        self.select_route_path(authority.map(Authority::as_str), uri.path(), method)
    }

    pub(crate) fn selects_on_host(&self) -> bool {
        self.routes.selects_on_host()
    }

    /// The plan of the route a request is served by, for its authority as written and its path. A path with no single
    /// meaning selects no route.
    pub(crate) fn select_route_path(
        &self,
        authority: Option<&str>,
        path: &str,
        method: &Method,
    ) -> Option<&Arc<HttpRoutePlan>> {
        let order = self.routes.select_index(authority, path, method)?;
        Some(&self.route_plans[order])
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

    /// Whether any route can be served on the fast lane; a service without one never pays for the lane's look at a
    /// request that the classic path serves.
    pub(crate) fn has_lane_route(&self) -> bool {
        self.route_plans
            .iter()
            .any(|route| route.lane != LaneRoute::Classic)
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
        .any(|pool| pool.health_check.is_some() || pool.canary.is_some())
    {
        return Err(ServicePlanError::HealthSupervisorRequired);
    }
    let plan = runtime_plan(config)?;
    let mut acquired = acquire_runtime_services(&plan)?;
    prepare_rtmp_service_specs(acquired.services(), RtmpPrepareMode::Activation)?;
    Ok(acquired.commit().0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::action::{FixedResponsePlan, RoutePolicyPlan};

    fn route(lane: LaneRoute, route_id: &str) -> Arc<HttpRoutePlan> {
        Arc::new(HttpRoutePlan {
            access: None,
            lane,
            edits: crate::http::proxy::lane::RouteEdits::default(),
            action: HttpActionPlan::Fixed(FixedResponsePlan::compile(200, "ok", &[])),
            policy: RoutePolicyPlan::compile(oxiroute_config::HttpRoutePolicy::default()),
            route_id: route_id.into(),
        })
    }

    fn service(lanes: &[LaneRoute]) -> HttpServicePlan {
        let routes = lanes.iter().map(|lane| route(*lane, "r")).collect();
        HttpServicePlan::new(None, routes, Duration::from_secs(1), RouteTable::default())
    }

    #[test]
    fn a_service_of_classic_routes_has_no_lane_route() {
        assert!(!service(&[LaneRoute::Classic, LaneRoute::Classic]).has_lane_route());
        assert!(!service(&[]).has_lane_route());
    }

    #[test]
    fn one_direct_or_proxied_route_is_a_lane_route() {
        assert!(service(&[LaneRoute::Classic, LaneRoute::Direct]).has_lane_route());
        assert!(service(&[LaneRoute::Proxy]).has_lane_route());
    }

    #[test]
    fn an_access_log_does_not_keep_fixed_responses_off_the_lane() {
        let directory = tempfile::tempdir().expect("log directory");
        let policy = oxiroute_config::AccessLogPolicy::File {
            path: directory.path().join("access.log"),
        };
        let mut logged = service(&[LaneRoute::Direct]);
        logged.access_log = AccessLog::open("s", Some(&policy))
            .expect("access log")
            .map(Arc::new);
        assert!(logged.has_lane_route());
        assert!(
            logged
                .route_plans
                .iter()
                .all(|route| route.lane == LaneRoute::Direct)
        );
    }

    #[test]
    fn the_route_a_table_selects_is_the_plan_at_its_position() {
        let routes = (0..12)
            .map(|index| {
                let path = oxiroute_config::HttpPathSelector::Exact {
                    value: format!("/{index}"),
                };
                crate::Route::new(None, path, None, index.to_string()).expect("route")
            })
            .collect();
        let plans = (0..12).map(|index| route(LaneRoute::Direct, &index.to_string()));
        let service = HttpServicePlan::new(
            None,
            plans.collect(),
            Duration::from_secs(1),
            RouteTable::new(routes),
        );
        for index in 0..12 {
            let plan = service.select_route_path(None, &format!("/{index}"), &Method::GET);
            assert_eq!(
                plan.map(|plan| &*plan.route_id),
                Some(index.to_string().as_str())
            );
        }
        assert!(
            service
                .select_route_path(None, "/12", &Method::GET)
                .is_none()
        );
    }
}
