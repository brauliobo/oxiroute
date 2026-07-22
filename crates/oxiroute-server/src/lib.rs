use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use http::{Method, Uri, header::HOST, uri::Authority};
use log::warn;
use oxiroute_config::{Config, Protocol, UpstreamAlgorithm, is_unambiguous_http_path};
use pingora::{
    Error, ErrorType,
    apps::ServerApp,
    protocols::Stream,
    proxy::{ProxyHttp, Session},
    server::ShutdownWatch,
    upstreams::peer::HttpPeer,
};

mod monitoring;
mod routing;
mod rtmp_api;
mod tcp_relay;

pub use monitoring::{
    ConnectionGuard, HostSnapshot, ListenerMetrics, ListenerSnapshot, MetricsError,
    ProcessSnapshot, RuntimeMetrics, RuntimeSnapshot, TrafficSnapshot,
};
pub use routing::{PoolError, RoundRobinPool, Route, RouteError, RouteTable};
pub use rtmp_api::{ApiResponse, RtmpManagementApi};
pub use tcp_relay::{
    RELAY_BUFFER_SIZE, RelayDirection, RelayFailure, RelayFailureKind, RelayOperation, RelayPolicy,
    RelayStats, TcpRelayCore, relay_streams,
};

pub struct HttpReverseProxy {
    metrics: ListenerMetrics,
    service: Arc<HttpServicePlan>,
}

impl HttpReverseProxy {
    #[must_use]
    pub fn new(service: Arc<HttpServicePlan>, metrics: ListenerMetrics) -> Self {
        Self { metrics, service }
    }
}

pub struct MonitoredHttpApp<A> {
    inner: Arc<A>,
    metrics: ListenerMetrics,
}

impl<A> MonitoredHttpApp<A> {
    #[must_use]
    pub fn new(inner: A, metrics: ListenerMetrics) -> Self {
        Self {
            inner: Arc::new(inner),
            metrics,
        }
    }
}

#[async_trait]
impl<A> ServerApp for MonitoredHttpApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let _connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected HTTP connection: {error}");
                return None;
            }
        };
        self.inner.process_new(downstream, shutdown).await
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

pub struct HttpRequestContext {
    listener: ListenerMetrics,
    observed_received: u64,
    observed_sent: u64,
    authority: Option<Authority>,
    upstream: Option<SocketAddr>,
}

impl HttpRequestContext {
    fn observe(&mut self, session: &Session) {
        observe_counter(
            &self.listener,
            session.body_bytes_read(),
            &mut self.observed_received,
            true,
        );
        observe_counter(
            &self.listener,
            session.body_bytes_sent(),
            &mut self.observed_sent,
            false,
        );
    }
}

#[async_trait]
impl ProxyHttp for HttpReverseProxy {
    type CTX = HttpRequestContext;

    fn new_ctx(&self) -> Self::CTX {
        HttpRequestContext {
            listener: self.metrics.clone(),
            observed_received: 0,
            observed_sent: 0,
            authority: None,
            upstream: None,
        }
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        let (authority, content_length, method, uri) = {
            let request = session.req_header();
            let authority = request_authority(request);
            let content_length = request
                .headers
                .get(http::header::CONTENT_LENGTH)
                .map(|value| {
                    value
                        .to_str()
                        .ok()
                        .and_then(|value| value.parse::<u64>().ok())
                });
            (
                authority,
                content_length,
                request.method.clone(),
                request.uri.clone(),
            )
        };

        let Ok(authority) = authority else {
            session.respond_error(400).await?;
            return Ok(true);
        };
        if content_length == Some(None) || !is_unambiguous_http_path(uri.path()) {
            session.respond_error(400).await?;
            return Ok(true);
        }
        if content_length
            .flatten()
            .is_some_and(|length| length > self.service.max_request_body_bytes())
        {
            session.respond_error(413).await?;
            return Ok(true);
        }

        let Some(upstream) = self.service.select(authority.as_ref(), &uri, &method) else {
            session
                .respond_error_with_body(404, Bytes::from_static(b"route not found\n"))
                .await?;
            return Ok(true);
        };
        ctx.authority = authority;
        ctx.upstream = Some(upstream);
        Ok(false)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let Some(upstream) = ctx.upstream else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        let mut peer = HttpPeer::new(upstream, false, String::new());
        let timeout = self.service.upstream_io_timeout();
        peer.options.connection_timeout = Some(timeout);
        peer.options.total_connection_timeout = Some(timeout);
        peer.options.read_timeout = Some(timeout);
        peer.options.write_timeout = Some(timeout);
        Ok(Box::new(peer))
    }

    async fn request_body_filter(
        &self,
        session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        ctx.observe(session);
        if session.was_upgraded() {
            return Ok(());
        }
        if u64::try_from(session.body_bytes_read())
            .is_ok_and(|bytes| bytes > self.service.max_request_body_bytes())
        {
            return Err(Error::new_down(ErrorType::HTTPStatus(413)));
        }
        Ok(())
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        if let Some(authority) = &ctx.authority {
            upstream_request.insert_header(HOST, authority.as_str())?;
        }
        Ok(())
    }

    fn response_body_filter(
        &self,
        session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Option<Duration>> {
        ctx.observe(session);
        Ok(None)
    }

    async fn logging(
        &self,
        session: &mut Session,
        _error: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        ctx.observe(session);
    }
}

fn request_authority(request: &pingora::http::RequestHeader) -> Result<Option<Authority>, ()> {
    let uri_authority = request.uri.authority().cloned();
    let mut host_values = request.headers.get_all(HOST).iter();
    let host_authority = host_values
        .next()
        .map(|host| {
            host.to_str()
                .map_err(|_| ())?
                .parse::<Authority>()
                .map_err(|_| ())
        })
        .transpose()?;
    if host_values.next().is_some() {
        return Err(());
    }
    if uri_authority
        .iter()
        .chain(host_authority.iter())
        .any(|authority| authority.as_str().contains('@'))
    {
        return Err(());
    }

    match (uri_authority, host_authority) {
        (Some(uri), Some(host)) if uri != host => Err(()),
        (Some(uri), _) => Ok(Some(uri)),
        (None, Some(host)) => Ok(Some(host)),
        (None, None) if request.version == http::Version::HTTP_11 => Err(()),
        (None, None) => Ok(None),
    }
}

fn observe_counter(listener: &ListenerMetrics, current: usize, observed: &mut u64, received: bool) {
    let Ok(current) = u64::try_from(current) else {
        warn!("HTTP byte counter exceeds the supported range");
        return;
    };
    let Some(delta) = current.checked_sub(*observed) else {
        warn!("HTTP byte counter moved backwards");
        return;
    };
    if delta == 0 {
        return;
    }
    let result = if received {
        listener.record_bytes_received(delta)
    } else {
        listener.record_bytes_sent(delta)
    };
    match result {
        Ok(()) => *observed = current,
        Err(error) => warn!("could not account for HTTP traffic: {error}"),
    }
}

#[derive(Clone, Debug)]
pub struct ServiceSpec {
    pub name: String,
    pub bind: SocketAddr,
    pub max_connections: u64,
    pub kind: ServiceKind,
}

#[derive(Clone, Debug)]
pub enum ServiceKind {
    Http(Arc<HttpServicePlan>),
    Rtmp,
    Tcp(Arc<L4ServicePlan>),
}

impl ServiceKind {
    #[must_use]
    pub const fn protocol(&self) -> &'static str {
        match self {
            Self::Http(_) => "http",
            Self::Rtmp => "rtmp",
            Self::Tcp(_) => "tcp",
        }
    }
}

#[derive(Debug)]
pub struct HttpServicePlan {
    max_request_body_bytes: u64,
    pools: Arc<HashMap<String, Arc<RoundRobinPool>>>,
    upstream_io_timeout: Duration,
    routes: RouteTable,
}

impl HttpServicePlan {
    #[must_use]
    pub fn select(
        &self,
        authority: Option<&Authority>,
        uri: &Uri,
        method: &Method,
    ) -> Option<SocketAddr> {
        let route = self.routes.select(authority, uri, method)?;
        self.pools.get(route.pool_id()).map(|pool| pool.select())
    }

    #[must_use]
    pub const fn upstream_io_timeout(&self) -> Duration {
        self.upstream_io_timeout
    }

    #[must_use]
    pub const fn max_request_body_bytes(&self) -> u64 {
        self.max_request_body_bytes
    }
}

#[derive(Debug)]
pub struct L4ServicePlan {
    policy: RelayPolicy,
    pool: Arc<RoundRobinPool>,
}

impl L4ServicePlan {
    #[must_use]
    pub const fn policy(&self) -> RelayPolicy {
        self.policy
    }

    #[must_use]
    pub fn select(&self) -> SocketAddr {
        self.pool.select()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServicePlanError {
    #[error("upstream pool `{pool}` cannot be compiled: {source}")]
    Pool { pool: String, source: PoolError },
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
    #[error("HTTP service `{service}` route {route} references unknown pool `{pool}`")]
    UnknownHttpPool {
        service: String,
        route: usize,
        pool: String,
    },
    #[error("listener `{listener}` requires a configured service")]
    MissingListenerService { listener: String },
    #[error("HTTP listener `{listener}` references unknown service `{service}`")]
    UnknownHttpService { listener: String, service: String },
    #[error("TCP listener `{listener}` references unknown service `{service}`")]
    UnknownL4Service { listener: String, service: String },
    #[error("RTMP listener `{listener}` must not reference a service")]
    UnexpectedRtmpService { listener: String },
    #[error("L4 service `{service}` references unknown pool `{pool}`")]
    UnknownL4Pool { service: String, pool: String },
}

/// Compiles validated listener definitions into runtime service specifications.
///
/// # Errors
///
/// Returns an error when a programmatically constructed configuration contains invalid routes,
/// pools, service references, or listener/service protocol relationships.
pub fn service_specs(config: &Config) -> Result<Vec<ServiceSpec>, ServicePlanError> {
    let pools = compile_pools(config)?;
    let http_services = compile_http_services(config, &pools)?;
    let l4_services = compile_l4_services(config, &pools)?;

    config
        .listeners
        .iter()
        .map(|listener| compile_listener(listener, &http_services, &l4_services))
        .collect()
}

fn compile_pools(
    config: &Config,
) -> Result<Arc<HashMap<String, Arc<RoundRobinPool>>>, ServicePlanError> {
    let mut pools = HashMap::with_capacity(config.upstream_pools.len());
    for pool in &config.upstream_pools {
        match pool.algorithm {
            UpstreamAlgorithm::RoundRobin => {}
        }
        let compiled = RoundRobinPool::new(pool.endpoints.iter().copied()).map_err(|source| {
            ServicePlanError::Pool {
                pool: pool.name.clone(),
                source,
            }
        })?;
        pools.insert(pool.name.clone(), Arc::new(compiled));
    }
    Ok(Arc::new(pools))
}

fn compile_http_services(
    config: &Config,
    pools: &Arc<HashMap<String, Arc<RoundRobinPool>>>,
) -> Result<HashMap<String, Arc<HttpServicePlan>>, ServicePlanError> {
    let mut http_services = HashMap::with_capacity(config.http_services.len());
    for service in &config.http_services {
        let routes = service
            .routes
            .iter()
            .enumerate()
            .map(|(route_index, route)| {
                if !pools.contains_key(&route.upstream_pool) {
                    return Err(ServicePlanError::UnknownHttpPool {
                        service: service.name.clone(),
                        route: route_index,
                        pool: route.upstream_pool.clone(),
                    });
                }
                let methods = if route.methods.is_empty() {
                    None
                } else {
                    Some(
                        route
                            .methods
                            .iter()
                            .map(|method| {
                                method.parse::<Method>().map_err(|_| {
                                    ServicePlanError::InvalidMethod {
                                        service: service.name.clone(),
                                        route: route_index,
                                        method: method.clone(),
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                };
                Route::new(
                    route.host.as_deref(),
                    &route.path_prefix,
                    methods,
                    &route.upstream_pool,
                )
                .map_err(|source| ServicePlanError::Route {
                    service: service.name.clone(),
                    route: route_index,
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        http_services.insert(
            service.name.clone(),
            Arc::new(HttpServicePlan {
                max_request_body_bytes: service.max_request_body_bytes,
                pools: Arc::clone(pools),
                upstream_io_timeout: Duration::from_millis(service.upstream_io_timeout_ms),
                routes: RouteTable::new(routes),
            }),
        );
    }
    Ok(http_services)
}

fn compile_l4_services(
    config: &Config,
    pools: &Arc<HashMap<String, Arc<RoundRobinPool>>>,
) -> Result<HashMap<String, Arc<L4ServicePlan>>, ServicePlanError> {
    let mut l4_services = HashMap::with_capacity(config.l4_services.len());
    for service in &config.l4_services {
        let Some(pool) = pools.get(&service.upstream_pool) else {
            return Err(ServicePlanError::UnknownL4Pool {
                service: service.name.clone(),
                pool: service.upstream_pool.clone(),
            });
        };
        l4_services.insert(
            service.name.clone(),
            Arc::new(L4ServicePlan {
                policy: RelayPolicy {
                    connect: Duration::from_millis(service.connect_timeout_ms),
                    idle: Some(Duration::from_millis(service.idle_timeout_ms)),
                    lifetime: service.lifetime_timeout_ms.map(Duration::from_millis),
                },
                pool: Arc::clone(pool),
            }),
        );
    }
    Ok(l4_services)
}

fn compile_listener(
    listener: &oxiroute_config::Listener,
    http_services: &HashMap<String, Arc<HttpServicePlan>>,
    l4_services: &HashMap<String, Arc<L4ServicePlan>>,
) -> Result<ServiceSpec, ServicePlanError> {
    let kind = match (listener.protocol, listener.service.as_deref()) {
        (Protocol::Http | Protocol::Tcp, None) => {
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
        (Protocol::Rtmp, None) => ServiceKind::Rtmp,
        (Protocol::Rtmp, Some(_)) => {
            return Err(ServicePlanError::UnexpectedRtmpService {
                listener: listener.name.clone(),
            });
        }
    };
    Ok(ServiceSpec {
        name: listener.name.clone(),
        bind: listener.bind,
        max_connections: listener.max_connections,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    struct CleanupApp {
        cleaned: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ServerApp for CleanupApp {
        async fn process_new(
            self: &Arc<Self>,
            _session: Stream,
            _shutdown: &ShutdownWatch,
        ) -> Option<Stream> {
            None
        }

        async fn cleanup(&self) {
            self.cleaned.store(true, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn monitored_http_app_delegates_cleanup() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_listener("http", "http", "127.0.0.1:8080", 100)
            .expect("listener metrics");
        let app = MonitoredHttpApp::new(
            CleanupApp {
                cleaned: Arc::clone(&cleaned),
            },
            metrics,
        );

        app.cleanup().await;

        assert!(cleaned.load(Ordering::Relaxed));
    }
}
