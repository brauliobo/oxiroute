use std::{
    collections::{HashSet, VecDeque},
    convert::Infallible,
    error::Error,
    future::{Future as _, poll_fn},
    io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::{Buf as _, Bytes, BytesMut};
use hickory_resolver::{
    TokioAsyncResolver,
    config::{LookupIpStrategy, NameServerConfigGroup, ResolverConfig, ResolverOpts},
};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri, header};
use http_body_util::{BodyExt as _, Full, Limited, combinators::BoxBody};
use hyper::body::{Body, Frame, Incoming};
use hyper_util::rt::TokioIo;
use openssl::{
    sha::Sha256,
    ssl::{SslConnector, SslMethod, SslVerifyMode},
};
use oxiroute_acme::ChallengeStore;
use oxiroute_cache::{
    CacheResponse, CachedResponse, FillOutcome, Lookup, ResponseTiming, StoreOutcome, Validators,
};
use oxiroute_config::{
    ForwardAccessAction, ForwardAccessCondition, ForwardAccessMatcher, ForwardAccessPolicy,
    ForwardAuditMode, ForwardDirectFallback, ForwardHeaderPolicy, ForwardHttpVersion, ForwardPeer,
    ForwardProxyAuth, ForwardProxyService, ForwardTimeRange, ForwardViaPolicy, ForwardWeekday,
    ForwardedForPolicy,
};
use oxiroute_forward_proxy::{
    ApprovedDestination, BoundedTunnel, Destination, DestinationRules, ForwardScheme,
    H2TunnelStream, Host, PolicyContext, Principal, Protocol, RuleError, TimeWindow, TunnelLimits,
    parse_absolute_form, parse_connect_authority, sanitize_request_headers,
};
use pingora::{
    apps::{HttpServerApp, HttpServerOptions, ReusedHttpStream},
    protocols::http::{
        ServerSession,
        v2::server::{H2Options, default_h2_options},
    },
    server::ShutdownWatch,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time::{Instant, Sleep, timeout_at},
};
use tokio_openssl::SslStream;

use crate::{
    H3UpstreamError, H3UpstreamPlan,
    http_action::{BasicHtpasswdAccess, CacheFill, CacheFillJoin, CacheRequest, HttpCachePlan},
    monitoring::CacheEvent,
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
};

type BoxError = Box<dyn Error + Send + Sync>;
pub type ForwardProxyBody = BoxBody<Bytes, BoxError>;
const MAX_BASIC_CREDENTIAL_CACHE_ENTRIES: usize = 4_096;

pub fn challenge_response(
    request: &Request<Incoming>,
    store: &ChallengeStore,
) -> Option<Response<ForwardProxyBody>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let response = store.route(request.method().as_str(), request.uri().path(), now)?;
    let body = if request.method() == Method::HEAD {
        Bytes::new()
    } else {
        Bytes::from(response.body)
    };
    let body = Full::new(body)
        .map_err(|never| -> BoxError { match never {} })
        .boxed();
    Response::builder()
        .status(response.status)
        .header(header::CONTENT_TYPE, response.content_type)
        .header(header::CACHE_CONTROL, response.cache_control)
        .body(body)
        .ok()
}

#[derive(Default)]
pub struct ForwardConnectionLifecycle {
    finished: tokio::sync::Notify,
    started: AtomicBool,
}

impl ForwardConnectionLifecycle {
    fn start(self: &Arc<Self>) -> ForwardTunnelCompletion {
        self.started.store(true, Ordering::Release);
        ForwardTunnelCompletion(Arc::clone(self))
    }

    pub async fn wait_if_started(&self) {
        if self.started.load(Ordering::Acquire) {
            self.finished.notified().await;
        }
    }
}

struct ForwardTunnelCompletion(Arc<ForwardConnectionLifecycle>);

impl Drop for ForwardTunnelCompletion {
    fn drop(&mut self) {
        self.0.finished.notify_one();
    }
}

trait ProxyIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ProxyIo for T {}
type BoxedIo = Box<dyn ProxyIo>;

enum ForwardAuthPlan {
    Bearer(SecureBearerToken),
    Basic(Box<ForwardBasicAuth>),
}

struct ForwardBasicAuth {
    access: Arc<BasicHtpasswdAccess>,
    cache: Mutex<VecDeque<CachedBasicCredential>>,
    cache_salt: [u8; 32],
    challenge: HeaderValue,
    path: PathBuf,
    realm: String,
    refresh: Mutex<()>,
    ttl: Option<Duration>,
    username_case_sensitive: bool,
}

struct CachedBasicCredential {
    digest: [u8; 32],
    principal: Arc<str>,
    validated_at: Instant,
}

impl ForwardBasicAuth {
    fn new(
        access: BasicHtpasswdAccess,
        path: PathBuf,
        realm: String,
        ttl: Option<Duration>,
        username_case_sensitive: bool,
    ) -> Result<Self, ForwardPlanError> {
        let mut cache_salt = [0; 32];
        openssl::rand::rand_bytes(&mut cache_salt).map_err(|_| ForwardPlanError::Authentication)?;
        let challenge = access.challenge().clone();
        Ok(Self {
            access: Arc::new(access),
            cache: Mutex::new(VecDeque::new()),
            cache_salt,
            challenge,
            path,
            realm,
            refresh: Mutex::new(()),
            ttl,
            username_case_sensitive,
        })
    }

    async fn authenticate(&self, credentials: &[u8]) -> Option<Arc<str>> {
        let Some(ttl) = self.ttl else {
            return self.access.authenticate(credentials).await;
        };
        let digest = self.credential_digest(credentials);
        {
            let mut cache = self.cache.lock().await;
            cache.retain(|entry| entry.validated_at.elapsed() < ttl);
            if let Some(entry) = cache.iter().find(|entry| entry.digest == digest) {
                return Some(Arc::clone(&entry.principal));
            }
        }

        let _refresh = self.refresh.lock().await;
        {
            let mut cache = self.cache.lock().await;
            cache.retain(|entry| entry.validated_at.elapsed() < ttl);
            if let Some(entry) = cache.iter().find(|entry| entry.digest == digest) {
                return Some(Arc::clone(&entry.principal));
            }
        }
        let path = self.path.clone();
        let realm = self.realm.clone();
        let username_case_sensitive = self.username_case_sensitive;
        let access = tokio::task::spawn_blocking(move || {
            BasicHtpasswdAccess::load_with_username_case(&path, &realm, username_case_sensitive)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .map(Arc::new)?;
        let principal = access.authenticate(credentials).await?;
        let mut cache = self.cache.lock().await;
        if cache.len() == MAX_BASIC_CREDENTIAL_CACHE_ENTRIES {
            cache.pop_front();
        }
        cache.push_back(CachedBasicCredential {
            digest,
            principal: Arc::clone(&principal),
            validated_at: Instant::now(),
        });
        Some(principal)
    }

    fn credential_digest(&self, credentials: &[u8]) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(&self.cache_salt);
        digest.update(credentials);
        digest.finish()
    }
}

pub struct ForwardHttp1ServicePlan {
    access_policy: Option<ForwardAccessPolicy>,
    allow_absolute_form: bool,
    audit_mode: ForwardAuditMode,
    auth: Option<ForwardAuthPlan>,
    challenge: Option<HeaderValue>,
    connect_enabled: bool,
    connect_ports: Arc<[u16]>,
    connect_timeout: Duration,
    destination_policy: DestinationRules,
    peer_direct_fallback: ForwardDirectFallback,
    peer_max_retries: usize,
    peers: Arc<[StaticPeerPlan]>,
    header_policy: ForwardHeaderPolicy,
    http_server_options: HttpServerOptions,
    idle_timeout: Duration,
    lifetime_timeout: Duration,
    local_addresses: Arc<HashSet<IpAddr>>,
    max_header_bytes: usize,
    max_request_body_bytes: usize,
    name: String,
    resolver: TokioAsyncResolver,
    resolver_addresses: usize,
    resolver_revalidate_on_connect: bool,
    resolver_queries: Arc<Semaphore>,
    service_connections: Arc<Semaphore>,
    tls_connector: Arc<SslConnector>,
    access_metrics: Arc<ForwardAccessMetrics>,
    cache: Option<Arc<HttpCachePlan>>,
    h3_upstream: Option<Arc<H3UpstreamPlan>>,
}

impl std::fmt::Debug for ForwardHttp1ServicePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ForwardHttp1ServicePlan")
            .field("name", &self.name)
            .field("auth", &self.auth.as_ref().map(|_| "configured"))
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ForwardPlanError {
    #[error("forward authentication failed secure preflight")]
    Authentication,
    #[error("forward destination policy could not be compiled")]
    DestinationPolicy,
    #[error("forward resolver could not be prepared")]
    Resolver,
    #[error("forward TLS client could not be prepared")]
    Tls,
    #[error("forward runtime limit exceeds this platform")]
    Limit,
    #[error("forward static peer could not be prepared")]
    Peer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestFailure {
    BadRequest,
    Authentication,
    Forbidden,
    BadGateway,
    GatewayTimeout,
    PayloadTooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardAccessResult {
    Allowed,
    BadRequest,
    Authentication,
    Forbidden,
    BadGateway,
    Timeout,
    PayloadTooLarge,
}

impl ForwardAccessResult {
    const ALL: [Self; 7] = [
        Self::Allowed,
        Self::BadRequest,
        Self::Authentication,
        Self::Forbidden,
        Self::BadGateway,
        Self::Timeout,
        Self::PayloadTooLarge,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Allowed => 0,
            Self::BadRequest => 1,
            Self::Authentication => 2,
            Self::Forbidden => 3,
            Self::BadGateway => 4,
            Self::Timeout => 5,
            Self::PayloadTooLarge => 6,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::BadRequest => "bad_request",
            Self::Authentication => "authentication",
            Self::Forbidden => "forbidden",
            Self::BadGateway => "bad_gateway",
            Self::Timeout => "timeout",
            Self::PayloadTooLarge => "payload_too_large",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ForwardAccessMetricsSnapshot {
    pub allowed: u64,
    pub bad_request: u64,
    pub authentication: u64,
    pub forbidden: u64,
    pub bad_gateway: u64,
    pub timeout: u64,
    pub payload_too_large: u64,
}

#[derive(Default)]
struct ForwardAccessMetrics {
    counts: [AtomicU64; ForwardAccessResult::ALL.len()],
}

impl ForwardAccessMetrics {
    fn record(&self, result: ForwardAccessResult) {
        let counter = &self.counts[result.index()];
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        });
    }

    fn snapshot(&self) -> ForwardAccessMetricsSnapshot {
        ForwardAccessMetricsSnapshot {
            allowed: self.counts[ForwardAccessResult::Allowed.index()].load(Ordering::Relaxed),
            bad_request: self.counts[ForwardAccessResult::BadRequest.index()]
                .load(Ordering::Relaxed),
            authentication: self.counts[ForwardAccessResult::Authentication.index()]
                .load(Ordering::Relaxed),
            forbidden: self.counts[ForwardAccessResult::Forbidden.index()].load(Ordering::Relaxed),
            bad_gateway: self.counts[ForwardAccessResult::BadGateway.index()]
                .load(Ordering::Relaxed),
            timeout: self.counts[ForwardAccessResult::Timeout.index()].load(Ordering::Relaxed),
            payload_too_large: self.counts[ForwardAccessResult::PayloadTooLarge.index()]
                .load(Ordering::Relaxed),
        }
    }
}

impl RequestFailure {
    const fn access_result(self) -> ForwardAccessResult {
        match self {
            Self::BadRequest => ForwardAccessResult::BadRequest,
            Self::Authentication => ForwardAccessResult::Authentication,
            Self::Forbidden => ForwardAccessResult::Forbidden,
            Self::BadGateway => ForwardAccessResult::BadGateway,
            Self::GatewayTimeout => ForwardAccessResult::Timeout,
            Self::PayloadTooLarge => ForwardAccessResult::PayloadTooLarge,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Authentication => "authentication",
            Self::Forbidden => "forbidden",
            Self::BadGateway => "bad_gateway",
            Self::GatewayTimeout => "timeout",
            Self::PayloadTooLarge => "payload_too_large",
        }
    }
}

impl RequestFailure {
    const fn status(self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Authentication => StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::BadGateway => StatusCode::BAD_GATEWAY,
            Self::GatewayTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum ForwardCacheDecision {
    Bypass,
    Respond(Response<ForwardProxyBody>),
    Continue(ForwardCacheState),
}

enum ParsedTarget {
    Forward(oxiroute_forward_proxy::ForwardTarget),
    Tunnel(Destination),
}

struct AuthorizedRequest {
    approved: ApprovedDestination,
    authenticated: bool,
    parsed: ParsedTarget,
    lifetime_deadline: Instant,
}

struct ForwardCacheRevalidation {
    key: oxiroute_cache::CacheKey,
    response: CachedResponse,
    validators: Validators,
    stale_if_error: bool,
}

struct ForwardCacheState {
    plan: Arc<HttpCachePlan>,
    request: CacheRequest,
    fill: Option<CacheFill>,
    listener: crate::ListenerMetrics,
    revalidation: Option<ForwardCacheRevalidation>,
    store_response: bool,
}

#[derive(Clone, Debug)]
struct StaticPeerPlan {
    host: Host,
    port: u16,
}

struct ConnectedHttp {
    stream: BoxedIo,
    via_peer: bool,
}

struct ForwardCacheCapture {
    plan: Arc<HttpCachePlan>,
    request: CacheRequest,
    fill: Option<CacheFill>,
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    tags: Vec<Bytes>,
    timing: ResponseTiming,
    admissible: bool,
    listener: crate::ListenerMetrics,
    store_response: bool,
}

struct ForwardCacheBody<B> {
    inner: B,
    capture: Option<ForwardCacheCapture>,
}

/// H2 uses the same compiled plan as H1; only the downstream stream adapter differs.
pub type ForwardHttp2ServicePlan = ForwardHttp1ServicePlan;

pub struct ForwardHttp2ServiceApp {
    service: Arc<ForwardHttp2ServicePlan>,
}

impl ForwardHttp2ServiceApp {
    #[must_use]
    pub fn new(service: Arc<ForwardHttp2ServicePlan>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl HttpServerApp for ForwardHttp2ServiceApp {
    async fn process_new_http(
        self: &Arc<Self>,
        session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        self.service.process_new_http(session, shutdown).await
    }

    fn h2_options(&self) -> Option<H2Options> {
        self.service.h2_options()
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        self.service.server_options()
    }
}

struct TimedBody<B> {
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    inner: B,
    lifetime: Pin<Box<Sleep>>,
}

struct RelayedRequestBody {
    inner: mpsc::Receiver<Frame<Bytes>>,
}

impl Body for RelayedRequestBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.inner)
            .poll_recv(context)
            .map(|frame| frame.map(Ok))
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_closed() && self.inner.is_empty()
    }
}

fn relay_request_body(
    body: Incoming,
    limit: usize,
    idle_timeout: Duration,
    lifetime_deadline: Instant,
) -> (
    RelayedRequestBody,
    oneshot::Receiver<Result<(), RequestFailure>>,
) {
    let (frames, receiver) = mpsc::channel(1);
    let (mut completion, completed) = oneshot::channel();
    tokio::spawn(async move {
        let mut body = Limited::new(body, limit);
        let idle = tokio::time::sleep(idle_timeout);
        let lifetime = tokio::time::sleep_until(lifetime_deadline);
        tokio::pin!(idle, lifetime);
        let outcome = loop {
            let frame = tokio::select! {
                () = completion.closed() => return,
                () = &mut idle => break Err(RequestFailure::GatewayTimeout),
                () = &mut lifetime => break Err(RequestFailure::GatewayTimeout),
                frame = poll_fn(|context| Pin::new(&mut body).poll_frame(context)) => frame,
            };
            let Some(frame) = frame else {
                break Ok(());
            };
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) if error.is::<http_body_util::LengthLimitError>() => {
                    break Err(RequestFailure::PayloadTooLarge);
                }
                Err(_) => break Err(RequestFailure::BadRequest),
            };
            idle.as_mut().reset(Instant::now() + idle_timeout);
            tokio::select! {
                () = completion.closed() => return,
                () = &mut idle => break Err(RequestFailure::GatewayTimeout),
                () = &mut lifetime => break Err(RequestFailure::GatewayTimeout),
                result = frames.send(frame) => {
                    let _ = result;
                },
            }
        };
        let _ = completion.send(outcome);
    });
    (RelayedRequestBody { inner: receiver }, completed)
}

impl<B> TimedBody<B> {
    fn new(inner: B, idle_timeout: Duration, lifetime_deadline: Instant) -> Self {
        Self {
            idle: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
            inner,
            lifetime: Box::pin(tokio::time::sleep_until(lifetime_deadline)),
        }
    }
}

impl<B> Body for TimedBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Error + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.lifetime.as_mut().poll(context).is_ready() {
            return Poll::Ready(Some(Err(Box::new(ForwardBodyTimeout::Lifetime))));
        }
        if self.idle.as_mut().poll(context).is_ready() {
            return Poll::Ready(Some(Err(Box::new(ForwardBodyTimeout::Idle))));
        }
        match Pin::new(&mut self.inner).poll_frame(context) {
            Poll::Ready(Some(Ok(frame))) => {
                let deadline = Instant::now() + self.idle_timeout;
                self.idle.as_mut().reset(deadline);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(Box::new(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

#[derive(Debug, thiserror::Error)]
enum ForwardBodyTimeout {
    #[error("forward response body exceeded its idle timeout")]
    Idle,
    #[error("forward response body exceeded its lifetime timeout")]
    Lifetime,
}

impl ForwardCacheState {
    fn complete_without_store(&mut self) {
        if let Some(fill) = self.fill.take() {
            let _ = fill.complete_without_store();
        }
    }

    fn take_capture(
        &mut self,
        status: StatusCode,
        headers: HeaderMap,
        body_complete: bool,
    ) -> Option<ForwardCacheCapture> {
        let fill = self.fill.take()?;
        Some(ForwardCacheCapture {
            plan: Arc::clone(&self.plan),
            request: self.request.clone(),
            fill: Some(fill),
            status,
            tags: self
                .plan
                .surrogate_header
                .as_ref()
                .map_or_else(Vec::new, |header| {
                    response_surrogate_tags_forward(&headers, header)
                }),
            headers,
            body: Vec::new(),
            timing: ResponseTiming {
                request_started: self.request.request_started,
                response_received: self.plan.cache.now(),
                response_received_wall: SystemTime::now(),
            },
            admissible: body_complete,
            listener: self.listener.clone(),
            store_response: self.store_response,
        })
    }

    async fn stale_response(&mut self) -> Option<CachedResponse> {
        let Some(revalidation) = self.revalidation.as_ref() else {
            self.complete_without_store();
            return None;
        };
        if !revalidation.stale_if_error {
            self.complete_without_store();
            return None;
        }
        let key = revalidation.key.clone();
        let stale = self.plan.cache.stale_if_error(&key).await.ok().flatten();
        self.complete_without_store();
        stale
    }
}

impl<B> Body for ForwardCacheBody<B>
where
    B: Body<Data = Bytes, Error = BoxError> + Unpin,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(context) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(capture) = &mut this.capture {
                    if let Some(data) = frame.data_ref() {
                        capture.record_data(data);
                    } else {
                        capture.admissible = false;
                    }
                }
                if this
                    .capture
                    .as_ref()
                    .is_some_and(ForwardCacheCapture::body_complete)
                {
                    if let Some(capture) = this.capture.take() {
                        tokio::spawn(finish_forward_cache_capture(capture));
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                if let Some(capture) = this.capture.take() {
                    capture.complete_without_store();
                }
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                if let Some(capture) = this.capture.take() {
                    tokio::spawn(finish_forward_cache_capture(capture));
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Drop for ForwardCacheBody<B> {
    fn drop(&mut self) {
        if let Some(capture) = self.capture.take() {
            if capture.body_complete() {
                tokio::spawn(finish_forward_cache_capture(capture));
            } else {
                capture.complete_without_store();
            }
        }
    }
}

impl ForwardCacheCapture {
    fn record_data(&mut self, data: &Bytes) {
        if !self.store_response || !self.admissible {
            return;
        }
        let limit = self.plan.cache.config().max_body_bytes;
        if data.len() > limit.saturating_sub(self.body.len()) {
            self.admissible = false;
            return;
        }
        self.body.extend_from_slice(data);
    }

    fn body_complete(&self) -> bool {
        if !self.store_response || !self.admissible {
            return false;
        }
        let mut lengths = self.headers.get_all(header::CONTENT_LENGTH).iter();
        let Some(length) = lengths.next() else {
            return false;
        };
        lengths.next().is_none()
            && length
                .to_str()
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                == Some(self.body.len())
    }

    fn complete_without_store(mut self) {
        if let Some(fill) = self.fill.take() {
            let _ = fill.complete_without_store();
        }
    }
}

async fn finish_forward_cache_capture(mut capture: ForwardCacheCapture) {
    let Some(fill) = capture.fill.take() else {
        return;
    };
    let tags_valid = capture.plan.cache_tags_within_limits(&capture.tags);
    if !capture.store_response
        || !capture.admissible
        || !tags_valid
        || !response_representation_valid_forward(
            capture.status,
            &capture.headers,
            capture.body.len(),
        )
    {
        let _ = fill.complete_without_store();
        return;
    }
    let tag_refs = capture.tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
    let prepared = capture.plan.cache.prepare_with_timeline(
        capture.request.representation_input(),
        CacheResponse {
            status: capture.status,
            headers: &capture.headers,
            body: Bytes::from(capture.body),
            timing: capture.timing,
            tags: &tag_refs,
        },
        &capture.plan.timeline,
    );
    let Ok(entry) = prepared else {
        let _ = fill.complete_without_store();
        return;
    };
    match fill.store(entry).await {
        Ok(StoreOutcome::Stored { evicted }) => {
            record_forward_cache_event(&capture.listener, CacheEvent::Admission);
            for _ in 0..evicted {
                record_forward_cache_event(&capture.listener, CacheEvent::Eviction);
            }
        }
        Ok(StoreOutcome::GenerationLost) | Err(_) => {}
    }
}

async fn wait_for_forward_fill(waiter: oxiroute_cache::FillWaiter, waits: &mut usize) -> bool {
    *waits = waits.saturating_add(1);
    if *waits > 2 {
        return false;
    }
    match waiter.wait().await {
        FillOutcome::Stored => true,
        FillOutcome::NotStored
        | FillOutcome::Cancelled
        | FillOutcome::Purged
        | FillOutcome::Filling => *waits < 2,
    }
}

async fn finish_forward_cache_revalidation(
    mut state: ForwardCacheState,
    headers: &HeaderMap,
) -> CachedResponse {
    let revalidation = state
        .revalidation
        .take()
        .expect("304 response has a forward cache revalidation");
    let timing = ResponseTiming {
        request_started: state.request.request_started,
        response_received: state.plan.cache.now(),
        response_received_wall: SystemTime::now(),
    };
    let stored = state.plan.cache.prepare_not_modified_with_timeline(
        state.request.representation_input(),
        &revalidation.key,
        headers,
        timing,
        &state.plan.timeline,
    );
    let listener = state.listener.clone();
    let mut admitted = false;
    if let Some(fill) = state.fill.take() {
        match stored {
            Ok(entry) => match fill.store(entry).await {
                Ok(StoreOutcome::Stored { evicted }) => {
                    admitted = true;
                    record_forward_cache_event(&listener, CacheEvent::Admission);
                    for _ in 0..evicted {
                        record_forward_cache_event(&listener, CacheEvent::Eviction);
                    }
                }
                Ok(StoreOutcome::GenerationLost) | Err(_) => {}
            },
            Err(_) => {
                let _ = fill.complete_without_store();
            }
        }
    }
    if admitted {
        match state.plan.cache.lookup(&state.request).await {
            Ok(Lookup::Hit { response, .. }) => response,
            _ => revalidation.response,
        }
    } else {
        revalidation.response
    }
}

fn cached_forward_response(
    response: CachedResponse,
    method: &Method,
) -> Response<ForwardProxyBody> {
    let body_forbidden = matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let body = if method == Method::HEAD || body_forbidden {
        Bytes::new()
    } else {
        response.body.clone()
    };
    let mut headers = response.headers;
    if matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
    ) {
        headers.remove(header::CONTENT_LENGTH);
    } else {
        let length = if response.status == StatusCode::RESET_CONTENT {
            0
        } else {
            response.body.len()
        };
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).expect("bounded cache body length"),
        );
    }
    let mut output = Response::builder()
        .status(response.status)
        .body(
            Full::new(body)
                .map_err(|never: Infallible| -> BoxError { match never {} })
                .boxed(),
        )
        .expect("cached forward response");
    *output.headers_mut() = headers;
    output
}

fn forward_response_from_parts(
    mut parts: http::response::Parts,
    body: Bytes,
    head: bool,
) -> Response<ForwardProxyBody> {
    if matches!(
        parts.status,
        StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
    ) {
        parts.headers.remove(header::CONTENT_LENGTH);
    } else if parts.status == StatusCode::RESET_CONTENT {
        parts
            .headers
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    }
    let body = if head { Bytes::new() } else { body };
    Response::from_parts(
        parts,
        Full::new(body)
            .map_err(|never: Infallible| -> BoxError { match never {} })
            .boxed(),
    )
}

fn response_representation_valid_forward(
    status: StatusCode,
    headers: &HeaderMap,
    body_len: usize,
) -> bool {
    if status.is_informational()
        || status == StatusCode::SWITCHING_PROTOCOLS
        || status == StatusCode::NOT_MODIFIED
    {
        return false;
    }
    if matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT) && body_len != 0 {
        return false;
    }
    let mut lengths = headers.get_all(header::CONTENT_LENGTH).iter();
    let Some(length) = lengths.next() else {
        return true;
    };
    lengths.next().is_none()
        && length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            == Some(body_len)
}

fn response_surrogate_tags_forward(headers: &HeaderMap, name: &http::HeaderName) -> Vec<Bytes> {
    headers
        .get_all(name)
        .iter()
        .flat_map(|value| {
            value
                .as_bytes()
                .split(u8::is_ascii_whitespace)
                .filter(|tag| !tag.is_empty())
                .map(Bytes::copy_from_slice)
        })
        .collect()
}

fn record_forward_cache_event(listener: &crate::ListenerMetrics, event: CacheEvent) {
    if let Err(error) = listener.record_cache_event(event) {
        log::warn!("could not account for forward cache metrics: {error}");
    }
}

async fn purge_forward_cache_base(
    plan: &HttpCachePlan,
    request: &CacheRequest,
) -> Result<oxiroute_cache::PurgeResult, crate::http_action::CacheBackendError> {
    let base = plan.cache.base(request)?;
    plan.cache.purge_base(&base).await
}

impl ForwardHttp1ServicePlan {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn compile_with_cache(
        service: &ForwardProxyService,
        cache: Option<Arc<HttpCachePlan>>,
    ) -> Result<Self, ForwardPlanError> {
        let auth = match &service.auth {
            Some(ForwardProxyAuth::BearerTokenFile { token_file_path }) => Some(
                SecureBearerToken::load(token_file_path)
                    .map(ForwardAuthPlan::Bearer)
                    .map_err(|_| ForwardPlanError::Authentication)?,
            ),
            Some(ForwardProxyAuth::BasicHtpasswdFile {
                htpasswd_file_path,
                realm,
                credential_ttl_ms,
                username_case_sensitive,
            }) => {
                let access = BasicHtpasswdAccess::load_with_username_case(
                    htpasswd_file_path,
                    realm,
                    *username_case_sensitive,
                )
                .map_err(|_| ForwardPlanError::Authentication)?;
                Some(ForwardAuthPlan::Basic(Box::new(ForwardBasicAuth::new(
                    access,
                    htpasswd_file_path.clone(),
                    realm.clone(),
                    credential_ttl_ms.map(Duration::from_millis),
                    *username_case_sensitive,
                )?)))
            }
            Some(ForwardProxyAuth::MutualTls { .. }) => {
                return Err(ForwardPlanError::Authentication);
            }
            None => None,
        };
        let challenge = match &auth {
            Some(ForwardAuthPlan::Bearer(_)) => Some(HeaderValue::from_static("Bearer")),
            Some(ForwardAuthPlan::Basic(auth)) => Some(auth.challenge.clone()),
            None => None,
        };
        let destination_policy = DestinationRules::new(
            service.destination_policy.allow_domains.clone(),
            service.destination_policy.deny_domains.clone(),
            service.destination_policy.allow_cidrs.clone(),
            service.destination_policy.deny_cidrs.clone(),
            service.destination_policy.deny_private,
        )
        .and_then(|policy| {
            policy.with_time_windows(
                destination_time_windows(&service.destination_policy.allow_times)?,
                destination_time_windows(&service.destination_policy.deny_times)?,
            )
        })
        .map_err(|_| ForwardPlanError::DestinationPolicy)?;
        let resolver = resolver(service)?;
        let peers = service
            .peer_policy
            .peers
            .iter()
            .map(static_peer_plan)
            .collect::<Result<Vec<_>, _>>()?;
        let mut connector =
            SslConnector::builder(SslMethod::tls_client()).map_err(|_| ForwardPlanError::Tls)?;
        connector.set_verify(SslVerifyMode::PEER);
        connector
            .set_default_verify_paths()
            .map_err(|_| ForwardPlanError::Tls)?;
        let max_connections =
            usize::try_from(service.max_connections).map_err(|_| ForwardPlanError::Limit)?;
        let max_header_bytes =
            usize::try_from(service.max_header_bytes).map_err(|_| ForwardPlanError::Limit)?;
        let max_request_body_bytes = usize::try_from(
            service
                .max_request_body_bytes
                .ok_or(ForwardPlanError::Limit)?,
        )
        .map_err(|_| ForwardPlanError::Limit)?;
        let h3_upstream = service
            .enabled_versions
            .contains(&ForwardHttpVersion::H3)
            .then(|| {
                H3UpstreamPlan::for_forward(max_connections)
                    .map(Arc::new)
                    .map_err(|_| ForwardPlanError::Tls)
            })
            .transpose()?;
        let resolver_addresses = usize::try_from(service.resolver.max_addresses_per_name)
            .map_err(|_| ForwardPlanError::Limit)?;
        let resolver_queries = usize::try_from(service.resolver.max_concurrent_queries)
            .map_err(|_| ForwardPlanError::Limit)?;
        let local_addresses = local_ip_address::list_afinet_netifas()
            .map_err(|_| ForwardPlanError::Resolver)?
            .into_iter()
            .map(|(_, address)| address)
            .collect();
        let mut http_server_options = HttpServerOptions::default();
        http_server_options.h2c = true;

        Ok(Self {
            access_policy: service.access_policy.clone(),
            allow_absolute_form: service.allow_absolute_form,
            audit_mode: service.audit_mode,
            auth,
            challenge,
            connect_enabled: service.connect.enabled,
            connect_ports: service.connect.allowed_ports.clone().into(),
            connect_timeout: Duration::from_millis(service.connect_timeout_ms),
            destination_policy,
            peer_direct_fallback: service.peer_policy.direct_fallback,
            peer_max_retries: usize::from(service.peer_policy.max_retries),
            peers: peers.into(),
            header_policy: service.header_policy.clone(),
            http_server_options,
            idle_timeout: Duration::from_millis(service.idle_timeout_ms),
            lifetime_timeout: Duration::from_millis(service.lifetime_timeout_ms),
            local_addresses: Arc::new(local_addresses),
            max_header_bytes,
            max_request_body_bytes,
            name: service.name.clone(),
            resolver,
            resolver_addresses,
            resolver_revalidate_on_connect: service.resolver.revalidate_on_connect,
            resolver_queries: Arc::new(Semaphore::new(resolver_queries)),
            service_connections: Arc::new(Semaphore::new(max_connections)),
            tls_connector: Arc::new(connector.build()),
            access_metrics: Arc::new(ForwardAccessMetrics::default()),
            cache,
            h3_upstream,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn max_header_bytes(&self) -> usize {
        self.max_header_bytes
    }

    #[must_use]
    pub const fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    #[must_use]
    pub const fn lifetime_timeout(&self) -> Duration {
        self.lifetime_timeout
    }

    pub fn begin_connection(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.service_connections)
            .try_acquire_owned()
            .ok()
    }

    fn h3_upstream(&self) -> Option<&H3UpstreamPlan> {
        self.h3_upstream.as_deref()
    }

    #[must_use]
    pub fn forward_access_metrics(&self) -> ForwardAccessMetricsSnapshot {
        self.access_metrics.snapshot()
    }

    async fn authorize_request<B>(
        &self,
        request: &Request<B>,
        protocol: Protocol,
        client_addr: Option<SocketAddr>,
    ) -> Result<AuthorizedRequest, RequestFailure> {
        let result = self
            .authorize_request_inner(request, protocol, client_addr)
            .await;
        let access_result = result.as_ref().map_or_else(
            |error| error.access_result(),
            |_| ForwardAccessResult::Allowed,
        );
        self.access_metrics.record(access_result);
        if self.audit_mode == ForwardAuditMode::Metadata {
            let destination = result
                .as_ref()
                .ok()
                .map(|request| request.approved.destination.authority());
            let authenticated = result
                .as_ref()
                .ok()
                .is_some_and(|request| request.authenticated);
            let event = serde_json::json!({
                "event": "forward_access",
                "service": self.name,
                "protocol": protocol_name(protocol),
                "method": request.method().as_str(),
                "destination": destination,
                "result": access_result.as_str(),
                "reason": result
                    .as_ref()
                    .err()
                    .map_or("authorized", |error| error.reason()),
                "status": result.as_ref().err().map(|error| error.status().as_u16()),
                "authenticated": authenticated,
                "clientIp": client_addr.map(|address| address.ip().to_string()),
            });
            log::info!(target: "oxiroute::forward_proxy", "{event}");
        }
        result
    }

    async fn authorize_request_inner<B>(
        &self,
        request: &Request<B>,
        protocol: Protocol,
        client_addr: Option<SocketAddr>,
    ) -> Result<AuthorizedRequest, RequestFailure> {
        let target = request.uri().to_string();
        let parsed = if request.method() == Method::CONNECT {
            parse_connect_authority(&target).map(ParsedTarget::Tunnel)
        } else if self.allow_absolute_form {
            parse_absolute_form(&target).map(ParsedTarget::Forward)
        } else {
            return Err(RequestFailure::Forbidden);
        };
        let parsed = parsed.map_err(|_| RequestFailure::BadRequest)?;
        let destination = match &parsed {
            ParsedTarget::Forward(target) => &target.destination,
            ParsedTarget::Tunnel(destination) => destination,
        };
        let lifetime_deadline = Instant::now() + self.lifetime_timeout;
        if matches!(parsed, ParsedTarget::Tunnel(_))
            && (!self.connect_enabled || !self.connect_ports.contains(&destination.port))
        {
            return Err(RequestFailure::Forbidden);
        }
        if self.pre_resolution_denied(request, client_addr, destination) {
            return Err(RequestFailure::Forbidden);
        }
        let principal = if self.auth.is_some()
            && !self.anonymous_access_possible(request, client_addr, destination)
        {
            match timeout_at(lifetime_deadline, self.authenticate(request.headers())).await {
                Ok(Ok(principal)) => Some(principal),
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            }
        } else {
            None
        };
        let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
        let addresses = match timeout_at(connect_deadline, self.resolve(destination)).await {
            Ok(Ok(addresses)) => addresses,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        };
        let addresses = if self.resolver_revalidate_on_connect {
            match timeout_at(connect_deadline, self.resolve(destination)).await {
                Ok(Ok(addresses)) => addresses,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            }
        } else {
            addresses
        };
        let principal = match timeout_at(
            lifetime_deadline,
            self.authorize_access(request, client_addr, destination, &addresses, principal),
        )
        .await
        {
            Ok(Ok(principal)) => principal,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        };
        let policy_principal = principal
            .clone()
            .unwrap_or_else(|| Principal::new("anonymous"));
        let approved = self
            .destination_policy
            .approve(
                &PolicyContext {
                    protocol,
                    principal: policy_principal,
                },
                destination,
                &addresses,
            )
            .map_err(|_| RequestFailure::Forbidden)?;
        Ok(AuthorizedRequest {
            approved,
            authenticated: principal.is_some(),
            parsed,
            lifetime_deadline,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub async fn handle(
        self: &Arc<Self>,
        mut request: Request<Incoming>,
        client_addr: Option<SocketAddr>,
        mut shutdown: ShutdownWatch,
        lifecycle: Arc<ForwardConnectionLifecycle>,
        listener: crate::ListenerMetrics,
    ) -> Response<ForwardProxyBody> {
        let AuthorizedRequest {
            approved,
            authenticated,
            parsed,
            lifetime_deadline,
            ..
        } = match self
            .authorize_request(&request, Protocol::Http1, client_addr)
            .await
        {
            Ok(request) => request,
            Err(error) => return self.rejection(error),
        };
        let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);

        match parsed {
            ParsedTarget::Tunnel(destination) => {
                let upstream = match timeout_at(
                    lifetime_deadline,
                    self.connect_tunnel_until(
                        &destination,
                        approved.socket_addresses.as_ref(),
                        connect_deadline,
                    ),
                )
                .await
                {
                    Ok(Ok(upstream)) => upstream,
                    Ok(Err(error)) => return self.rejection(error),
                    Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
                };
                let upgrade = hyper::upgrade::on(&mut request);
                let idle_timeout = self.idle_timeout;
                let lifetime_timeout = lifetime_deadline.saturating_duration_since(Instant::now());
                if lifetime_timeout.is_zero() {
                    return self.rejection(RequestFailure::GatewayTimeout);
                }
                let tunnel_destination = approved.destination.authority();
                let completion = lifecycle.start();
                tokio::spawn(async move {
                    let _completion = completion;
                    let upgraded = tokio::select! {
                        result = upgrade => result.ok(),
                        _ = shutdown.changed() => None,
                    };
                    if let Some(upgraded) = upgraded {
                        let Ok(tunnel) = BoundedTunnel::new(TunnelLimits {
                            idle_timeout,
                            lifetime_timeout,
                            ..TunnelLimits::default()
                        }) else {
                            return;
                        };
                        let outcome = tokio::select! {
                            outcome = tunnel.relay(TokioIo::new(upgraded), upstream) => Some(outcome),
                            _ = shutdown.changed() => None,
                        };
                        match outcome {
                            Some(outcome) => log::info!(
                                target: "oxiroute::forward_proxy",
                                "event=forward_tunnel protocol=h1 destination={} outcome={} bytes_left_to_right={} bytes_right_to_left={}",
                                tunnel_destination,
                                outcome.kind().as_str(),
                                outcome.stats().left_to_right,
                                outcome.stats().right_to_left,
                            ),
                            None => log::info!(
                                target: "oxiroute::forward_proxy",
                                "event=forward_tunnel protocol=h1 destination={tunnel_destination} outcome=cancelled bytes_left_to_right=0 bytes_right_to_left=0",
                            ),
                        }
                    }
                });
                response(StatusCode::OK, Bytes::new())
            }
            ParsedTarget::Forward(target) => {
                self.handle_forward_request(
                    request,
                    target,
                    approved,
                    authenticated,
                    lifetime_deadline,
                    connect_deadline,
                    shutdown,
                    listener,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn handle_forward_request(
        &self,
        mut request: Request<Incoming>,
        target: oxiroute_forward_proxy::ForwardTarget,
        approved: ApprovedDestination,
        authenticated: bool,
        lifetime_deadline: Instant,
        connect_deadline: Instant,
        mut shutdown: ShutdownWatch,
        listener: crate::ListenerMetrics,
    ) -> Response<ForwardProxyBody> {
        if request.body().size_hint().upper().is_some_and(|length| {
            length > u64::try_from(self.max_request_body_bytes).unwrap_or(u64::MAX)
        }) {
            return self.rejection(RequestFailure::PayloadTooLarge);
        }
        if request.method().as_str().eq_ignore_ascii_case("PURGE")
            && self
                .cache
                .as_ref()
                .is_some_and(|plan| plan.purge_access.is_some())
        {
            return self.handle_forward_cache_purge(request, &target).await;
        }

        let mut cache_state = match self
            .prepare_forward_cache(&request, &target, authenticated, &listener)
            .await
        {
            ForwardCacheDecision::Bypass => None,
            ForwardCacheDecision::Respond(response) => return response,
            ForwardCacheDecision::Continue(state) => Some(state),
        };
        let ConnectedHttp {
            stream: upstream,
            via_peer,
        } = match timeout_at(
            lifetime_deadline,
            self.connect_http_with_peers(
                &target.destination,
                target.scheme,
                approved.socket_addresses.as_ref(),
                connect_deadline,
            ),
        )
        .await
        {
            Ok(Ok(upstream)) => upstream,
            Ok(Err(error)) => {
                return self.forward_failure(&mut cache_state, error).await;
            }
            Err(_) => {
                return self
                    .forward_failure(&mut cache_state, RequestFailure::GatewayTimeout)
                    .await;
            }
        };
        let request_uri = if via_peer && target.scheme == ForwardScheme::Http {
            let Some(uri) = absolute_form(&target) else {
                return self
                    .forward_failure(&mut cache_state, RequestFailure::BadRequest)
                    .await;
            };
            uri
        } else {
            target.origin_form.clone()
        };
        *request.uri_mut() = request_uri;
        let Ok(mut headers) = sanitize_request_headers(request.headers(), &target.destination)
        else {
            return self
                .forward_failure(&mut cache_state, RequestFailure::BadRequest)
                .await;
        };
        apply_header_policy(&mut headers, &self.header_policy);
        if let Some(state) = cache_state.as_ref().filter(|state| state.plan.revalidate) {
            if let Some(validators) = state
                .revalidation
                .as_ref()
                .map(|revalidation| &revalidation.validators)
            {
                validators.apply(&mut headers);
            }
        }
        *request.headers_mut() = headers;
        let request_body_empty = request.body().size_hint().upper() == Some(0);
        let (parts, body) = request.into_parts();
        let (body, mut body_completion) = relay_request_body(
            body,
            self.max_request_body_bytes,
            self.idle_timeout,
            lifetime_deadline,
        );
        let request = Request::from_parts(parts, body);
        let handshake_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
        let (mut sender, connection) = match timeout_at(
            handshake_deadline,
            hyper::client::conn::http1::handshake(TokioIo::new(upstream)),
        )
        .await
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(_)) => {
                return self
                    .forward_failure(&mut cache_state, RequestFailure::BadGateway)
                    .await;
            }
            Err(_) => {
                return self
                    .forward_failure(&mut cache_state, RequestFailure::GatewayTimeout)
                    .await;
            }
        };
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let response = sender.send_request(request);
        tokio::pin!(response);
        let mut body_complete = request_body_empty;
        let response_idle = tokio::time::sleep(self.idle_timeout);
        tokio::pin!(response_idle);
        let mut upstream_response = loop {
            tokio::select! {
                biased;
                result = &mut body_completion, if !body_complete => {
                    body_complete = true;
                    match result {
                        Ok(Ok(())) => {
                            response_idle.as_mut().reset(Instant::now() + self.idle_timeout);
                        }
                        Ok(Err(error)) => {
                            return self.forward_failure(&mut cache_state, error).await;
                        }
                        Err(_) => {
                            return self
                                .forward_failure(&mut cache_state, RequestFailure::BadGateway)
                                .await;
                        }
                    }
                }
                result = &mut response => break match result {
                    Ok(response) => response,
                    Err(_) => {
                        return self
                            .forward_failure(&mut cache_state, RequestFailure::BadGateway)
                            .await;
                    }
                },
                () = &mut response_idle, if body_complete => {
                    return self
                        .forward_failure(&mut cache_state, RequestFailure::GatewayTimeout)
                        .await;
                }
                () = tokio::time::sleep_until(lifetime_deadline) => {
                    return self
                        .forward_failure(&mut cache_state, RequestFailure::GatewayTimeout)
                        .await;
                }
                _ = shutdown.changed() => {
                    return self
                        .forward_failure(&mut cache_state, RequestFailure::GatewayTimeout)
                        .await;
                }
            }
        };
        if sanitize_response_headers(upstream_response.headers_mut()).is_err() {
            return self
                .forward_failure(&mut cache_state, RequestFailure::BadGateway)
                .await;
        }
        if !body_complete {
            upstream_response
                .headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
            drop(body_completion);
        }

        let (parts, body) = upstream_response.into_parts();
        let timed_body = TimedBody::new(body, self.idle_timeout, lifetime_deadline);
        if parts.status == StatusCode::NOT_MODIFIED {
            match timeout_at(lifetime_deadline, timed_body.collect()).await {
                Ok(Ok(_)) => {}
                Ok(Err(_)) => {
                    return self
                        .forward_failure(&mut cache_state, RequestFailure::BadGateway)
                        .await;
                }
                Err(_) => {
                    return self
                        .forward_failure(&mut cache_state, RequestFailure::GatewayTimeout)
                        .await;
                }
            }
            if let Some(mut state) = cache_state.take() {
                if state.revalidation.is_some() {
                    let method = state.request.method.clone();
                    let response = finish_forward_cache_revalidation(state, &parts.headers).await;
                    return cached_forward_response(response, &method);
                }
                state.complete_without_store();
            }
            return forward_response_from_parts(parts, Bytes::new(), false);
        }

        let mut capture = None;
        if let Some(mut state) = cache_state.take() {
            if state.store_response {
                capture = state.take_capture(parts.status, parts.headers.clone(), body_complete);
            } else {
                state.complete_without_store();
            }
        }
        let body = match capture {
            Some(capture) => ForwardCacheBody {
                inner: timed_body,
                capture: Some(capture),
            }
            .boxed(),
            None => timed_body.boxed(),
        };
        Response::from_parts(parts, body)
    }

    async fn handle_forward_cache_purge(
        &self,
        request: Request<Incoming>,
        target: &oxiroute_forward_proxy::ForwardTarget,
    ) -> Response<ForwardProxyBody> {
        let Some(plan) = &self.cache else {
            return self.rejection(RequestFailure::BadRequest);
        };
        let Some(access) = &plan.purge_access else {
            return self.rejection(RequestFailure::BadRequest);
        };
        if !access.authorizes(request.headers()) {
            let mut response = response(StatusCode::UNAUTHORIZED, Bytes::new());
            response
                .headers_mut()
                .insert(header::PROXY_AUTHENTICATE, access.challenge().clone());
            return response;
        }
        if request
            .body()
            .size_hint()
            .upper()
            .is_some_and(|length| length > 0)
        {
            return response(StatusCode::BAD_REQUEST, Bytes::new());
        }
        let headers = request.headers().clone();
        let cache_request = CacheRequest {
            method: request.method().clone(),
            scheme: target.scheme.as_str(),
            authority: target.destination.authority(),
            path: target.origin_form.path().to_owned(),
            query: target.origin_form.query().map(str::to_owned),
            headers: headers.clone(),
            request_started: plan.cache.now(),
        };
        let result = if let Some(header_name) = &plan.surrogate_header {
            if let Some(value) = headers.get(header_name) {
                let bytes = value.as_bytes();
                if bytes.is_empty()
                    || !bytes
                        .iter()
                        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b'"'))
                {
                    return response(StatusCode::BAD_REQUEST, Bytes::new());
                }
                plan.cache.purge_tag(bytes).await
            } else {
                purge_forward_cache_base(plan, &cache_request).await
            }
        } else {
            purge_forward_cache_base(plan, &cache_request).await
        };
        let status = match result {
            Ok(result) if result.entries == 0 => StatusCode::NOT_FOUND,
            Ok(_) => StatusCode::OK,
            Err(error) => {
                log::warn!("forward cache purge failed: {error}");
                if error.is_invalid_request() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }
        };
        let mut output = response(status, Bytes::new());
        output.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        output
    }

    #[allow(clippy::too_many_lines)]
    async fn prepare_forward_cache(
        &self,
        request: &Request<Incoming>,
        target: &oxiroute_forward_proxy::ForwardTarget,
        authenticated: bool,
        listener: &crate::ListenerMetrics,
    ) -> ForwardCacheDecision {
        let Some(plan) = &self.cache else {
            return ForwardCacheDecision::Bypass;
        };
        if authenticated
            || !matches!(request.method(), &Method::GET | &Method::HEAD)
            || !plan.allows_method(request.method())
            || request.body().size_hint().upper() != Some(0)
            || request.headers().contains_key(header::AUTHORIZATION)
            || request.headers().contains_key(header::PROXY_AUTHORIZATION)
            || request.headers().contains_key(header::COOKIE)
        {
            return ForwardCacheDecision::Bypass;
        }
        let headers = request.headers().clone();
        let only_if_cached = oxiroute_cache::CacheControl::parse(&headers)
            .ok()
            .is_some_and(|control| control.only_if_cached);
        let cache_request = CacheRequest {
            method: request.method().clone(),
            scheme: target.scheme.as_str(),
            authority: target.destination.authority(),
            path: target.origin_form.path().to_owned(),
            query: target.origin_form.query().map(str::to_owned),
            headers,
            request_started: plan.cache.now(),
        };
        let mut waits = 0;
        loop {
            let lookup = match plan.cache.lookup(&cache_request).await {
                Ok(lookup) => lookup,
                Err(error) => {
                    if !error.is_invalid_request() {
                        log::warn!("forward cache lookup bypassed: {error}");
                    }
                    return ForwardCacheDecision::Bypass;
                }
            };
            match lookup {
                Lookup::Bypass { .. } => return ForwardCacheDecision::Bypass,
                Lookup::Hit { response, .. } => {
                    record_forward_cache_event(listener, CacheEvent::Hit);
                    return ForwardCacheDecision::Respond(cached_forward_response(
                        response,
                        request.method(),
                    ));
                }
                Lookup::Miss {
                    base,
                    only_if_cached: miss_only_if_cached,
                    ..
                } => {
                    record_forward_cache_event(listener, CacheEvent::Miss);
                    if only_if_cached || miss_only_if_cached {
                        return ForwardCacheDecision::Respond(response(
                            StatusCode::GATEWAY_TIMEOUT,
                            Bytes::new(),
                        ));
                    }
                    let fill = match plan.cache.begin_fill(base).await {
                        Ok(CacheFillJoin::Leader(fill)) => fill,
                        Ok(CacheFillJoin::Follower(waiter)) => {
                            if !wait_for_forward_fill(waiter, &mut waits).await {
                                return ForwardCacheDecision::Bypass;
                            }
                            continue;
                        }
                        Ok(CacheFillJoin::AtCapacity) | Err(_) => {
                            return ForwardCacheDecision::Bypass;
                        }
                    };
                    return ForwardCacheDecision::Continue(ForwardCacheState {
                        plan: Arc::clone(plan),
                        request: cache_request,
                        fill: Some(fill),
                        listener: listener.clone(),
                        revalidation: None,
                        store_response: request.method() == Method::GET,
                    });
                }
                Lookup::Revalidate {
                    response: cached,
                    validators,
                    stale_if_error,
                    ..
                } => {
                    record_forward_cache_event(listener, CacheEvent::Miss);
                    if only_if_cached {
                        return ForwardCacheDecision::Respond(response(
                            StatusCode::GATEWAY_TIMEOUT,
                            Bytes::new(),
                        ));
                    }
                    let base = cached.key.base().clone();
                    let fill = match plan.cache.begin_fill(base).await {
                        Ok(CacheFillJoin::Leader(fill)) => fill,
                        Ok(CacheFillJoin::Follower(waiter)) => {
                            if !wait_for_forward_fill(waiter, &mut waits).await {
                                return ForwardCacheDecision::Bypass;
                            }
                            continue;
                        }
                        Ok(CacheFillJoin::AtCapacity) | Err(_) => {
                            return ForwardCacheDecision::Bypass;
                        }
                    };
                    return ForwardCacheDecision::Continue(ForwardCacheState {
                        plan: Arc::clone(plan),
                        request: cache_request,
                        fill: Some(fill),
                        listener: listener.clone(),
                        revalidation: Some(ForwardCacheRevalidation {
                            key: cached.key.clone(),
                            response: cached,
                            validators,
                            stale_if_error,
                        }),
                        store_response: request.method() == Method::GET,
                    });
                }
            }
        }
    }

    async fn forward_failure(
        &self,
        cache_state: &mut Option<ForwardCacheState>,
        failure: RequestFailure,
    ) -> Response<ForwardProxyBody> {
        if let Some(state) = cache_state.as_mut() {
            let method = state.request.method.clone();
            if let Some(response) = state.stale_response().await {
                return cached_forward_response(response, &method);
            }
            state.complete_without_store();
        }
        self.rejection(failure)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn handle_h3<S>(
        &self,
        request: Request<()>,
        mut stream: h3::server::RequestStream<S, Bytes>,
        client_addr: Option<SocketAddr>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) where
        S: h3::quic::BidiStream<Bytes> + Send,
    {
        let AuthorizedRequest {
            approved,
            parsed,
            lifetime_deadline,
            ..
        } = match self
            .authorize_request(&request, Protocol::Http3, client_addr)
            .await
        {
            Ok(request) => request,
            Err(error) => {
                let _ = send_h3_failure(&mut stream, error, self.challenge.as_ref()).await;
                return;
            }
        };

        match parsed {
            ParsedTarget::Tunnel(_) => {
                let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
                let upstream = match timeout_at(
                    lifetime_deadline,
                    self.connect_tcp_until(approved.socket_addresses.as_ref(), connect_deadline),
                )
                .await
                {
                    Ok(Ok(upstream)) => upstream,
                    Ok(Err(error)) => {
                        let _ = send_h3_failure(&mut stream, error, self.challenge.as_ref()).await;
                        return;
                    }
                    Err(_) => {
                        let _ = send_h3_failure(
                            &mut stream,
                            RequestFailure::GatewayTimeout,
                            self.challenge.as_ref(),
                        )
                        .await;
                        return;
                    }
                };
                let Ok(response) = Response::builder().status(StatusCode::OK).body(()) else {
                    return;
                };
                if !matches!(
                    timeout_at(lifetime_deadline, stream.send_response(response)).await,
                    Ok(Ok(()))
                ) {
                    return;
                }
                let lifetime_timeout = lifetime_deadline.saturating_duration_since(Instant::now());
                let Ok(tunnel) = BoundedTunnel::new(TunnelLimits {
                    idle_timeout: self.idle_timeout,
                    lifetime_timeout,
                    ..TunnelLimits::default()
                }) else {
                    return;
                };
                let relay = tunnel.relay_h3(stream, upstream);
                tokio::pin!(relay);
                let outcome = tokio::select! {
                    outcome = &mut relay => Some(outcome),
                    _ = shutdown.changed() => None,
                };
                match outcome {
                    Some(outcome) => {
                        log::info!(
                            target: "oxiroute::forward_proxy",
                            "event=forward_tunnel protocol=h3 destination={} outcome={} bytes_left_to_right={} bytes_right_to_left={}",
                            approved.destination.authority(),
                            outcome.kind().as_str(),
                            outcome.stats().left_to_right,
                            outcome.stats().right_to_left,
                        );
                    }
                    None => log::info!(
                        target: "oxiroute::forward_proxy",
                        "event=forward_tunnel protocol=h3 destination={} outcome=cancelled bytes_left_to_right=0 bytes_right_to_left=0",
                        approved.destination.authority(),
                    ),
                }
            }
            ParsedTarget::Forward(target) => {
                let body_limit = u64::try_from(self.max_request_body_bytes).unwrap_or(u64::MAX);
                if request
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|length| length > body_limit)
                {
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::PayloadTooLarge,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                }
                let body = match recv_h3_body(
                    &mut stream,
                    self.max_request_body_bytes,
                    lifetime_deadline,
                    &mut shutdown,
                )
                .await
                {
                    Ok(body) => body,
                    Err(error) => {
                        let _ = send_h3_failure(&mut stream, error, self.challenge.as_ref()).await;
                        return;
                    }
                };
                if target.scheme != ForwardScheme::Https {
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadGateway,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                }
                let Some(h3_upstream) = self.h3_upstream() else {
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadGateway,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                let Ok(mut headers) =
                    sanitize_request_headers(request.headers(), &target.destination)
                else {
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadRequest,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                apply_header_policy(&mut headers, &self.header_policy);
                headers.remove(header::HOST);
                let mut parts = request.into_parts().0;
                let path = target
                    .origin_form
                    .path_and_query()
                    .map_or(target.origin_form.path(), |value| value.as_str());
                let Ok(uri) = Uri::builder()
                    .scheme("https")
                    .authority(target.destination.authority())
                    .path_and_query(path)
                    .build()
                else {
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadRequest,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                parts.uri = uri;
                parts.version = http::Version::HTTP_3;
                parts.headers = headers;
                let request = Request::from_parts(parts, ());
                let server_name = match &target.destination.host {
                    Host::Dns(host) => host.as_str().to_owned(),
                    Host::Ip(address) => address.to_string(),
                };
                let mut response = None;
                let mut last_error = H3UpstreamError::Connect;
                for address in approved.socket_addresses.iter() {
                    match h3_upstream
                        .request(
                            *address,
                            &server_name,
                            request.clone(),
                            body.clone(),
                            lifetime_deadline,
                            shutdown.clone(),
                        )
                        .await
                    {
                        Ok(value) => {
                            response = Some(value);
                            break;
                        }
                        Err(error) if error.retryable() => last_error = error,
                        Err(error) => {
                            last_error = error;
                            break;
                        }
                    }
                    if Instant::now() >= lifetime_deadline {
                        break;
                    }
                }
                let Some(response) = response else {
                    let failure = if matches!(last_error, H3UpstreamError::Timeout) {
                        RequestFailure::GatewayTimeout
                    } else {
                        RequestFailure::BadGateway
                    };
                    let _ = send_h3_failure(&mut stream, failure, self.challenge.as_ref()).await;
                    return;
                };
                let mut response_headers = response.headers;
                if sanitize_response_headers(&mut response_headers).is_err() {
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadGateway,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                }
                let status = response.status;
                let mut head = Response::new(());
                *head.status_mut() = status;
                *head.version_mut() = http::Version::HTTP_3;
                *head.headers_mut() = response_headers;
                if !matches!(
                    timeout_at(lifetime_deadline, stream.send_response(head)).await,
                    Ok(Ok(()))
                ) {
                    return;
                }
                if request.method() != Method::HEAD
                    && !matches!(
                        status,
                        StatusCode::NO_CONTENT
                            | StatusCode::RESET_CONTENT
                            | StatusCode::NOT_MODIFIED
                    )
                    && !response.body.is_empty()
                    && !matches!(
                        timeout_at(lifetime_deadline, stream.send_data(response.body)).await,
                        Ok(Ok(()))
                    )
                {
                    return;
                }
                if let Some(mut trailers) = response.trailers {
                    if sanitize_response_headers(&mut trailers).is_err() {
                        return;
                    }
                    if !matches!(
                        timeout_at(lifetime_deadline, stream.send_trailers(trailers)).await,
                        Ok(Ok(()))
                    ) {
                        return;
                    }
                }
                let _ = timeout_at(lifetime_deadline, stream.finish()).await;
            }
        }
    }

    async fn resolve(&self, destination: &Destination) -> Result<Vec<IpAddr>, RequestFailure> {
        match &destination.host {
            Host::Ip(address) => Ok(vec![*address]),
            Host::Dns(name) => {
                let _query = self
                    .resolver_queries
                    .acquire()
                    .await
                    .map_err(|_| RequestFailure::BadGateway)?;
                let lookup = self
                    .resolver
                    .lookup_ip(name)
                    .await
                    .map_err(|_| RequestFailure::BadGateway)?;
                let addresses = lookup
                    .iter()
                    .take(self.resolver_addresses)
                    .collect::<Vec<_>>();
                if lookup.iter().nth(self.resolver_addresses).is_some() {
                    return Err(RequestFailure::Forbidden);
                }
                (!addresses.is_empty())
                    .then_some(addresses)
                    .ok_or(RequestFailure::BadGateway)
            }
        }
    }

    async fn authorize_access<B>(
        &self,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
        addresses: &[IpAddr],
        mut principal: Option<Principal>,
    ) -> Result<Option<Principal>, RequestFailure> {
        let Some(policy) = &self.access_policy else {
            return if principal.is_some() {
                Ok(principal)
            } else if self.auth.is_some() {
                self.authenticate(request.headers()).await.map(Some)
            } else {
                Ok(None)
            };
        };
        if principal.is_none() && self.auth.is_some() {
            principal = self.authenticate(request.headers()).await.ok();
        }
        for rule in &policy.rules {
            let mut matched = true;
            for condition in &rule.conditions {
                let value = self.condition_matches(
                    condition,
                    request,
                    client_addr,
                    destination,
                    addresses,
                    principal.as_ref(),
                );
                if value == condition.negated {
                    matched = false;
                    break;
                }
            }
            if matched {
                return match rule.action {
                    ForwardAccessAction::Allow => Ok(principal),
                    ForwardAccessAction::Deny => Err(RequestFailure::Forbidden),
                };
            }
        }
        match policy.default_action {
            ForwardAccessAction::Allow => Ok(principal),
            ForwardAccessAction::Deny => Err(RequestFailure::Forbidden),
        }
    }

    fn pre_resolution_denied<B>(
        &self,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
    ) -> bool {
        let Some(policy) = &self.access_policy else {
            return false;
        };
        for rule in &policy.rules {
            let mut matched = true;
            let mut unknown = false;
            for condition in &rule.conditions {
                let value = match &condition.matcher {
                    ForwardAccessMatcher::All => Some(true),
                    ForwardAccessMatcher::Methods { methods } => Some(
                        methods
                            .iter()
                            .any(|method| method == request.method().as_str()),
                    ),
                    ForwardAccessMatcher::SourceCidrs { cidrs } => {
                        Some(source_cidrs_match(cidrs, client_addr))
                    }
                    ForwardAccessMatcher::DestinationPorts { ranges } => Some(
                        ranges
                            .iter()
                            .any(|range| (range.start..=range.end).contains(&destination.port)),
                    ),
                    ForwardAccessMatcher::Manager => Some(false),
                    ForwardAccessMatcher::Authenticated
                    | ForwardAccessMatcher::DestinationLocal
                    | ForwardAccessMatcher::DestinationLinkLocal => None,
                };
                match value {
                    Some(value) if value == condition.negated => {
                        matched = false;
                        break;
                    }
                    Some(_) => {}
                    None => unknown = true,
                }
            }
            if matched && unknown {
                return false;
            }
            if matched {
                return rule.action == ForwardAccessAction::Deny;
            }
        }
        policy.default_action == ForwardAccessAction::Deny
    }

    fn anonymous_access_possible<B>(
        &self,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
    ) -> bool {
        let Some(policy) = &self.access_policy else {
            return self.auth.is_none();
        };
        for rule in &policy.rules {
            let mut matched = true;
            let mut address_dependent = false;
            for condition in &rule.conditions {
                let value = match &condition.matcher {
                    ForwardAccessMatcher::All => Some(true),
                    ForwardAccessMatcher::Methods { methods } => Some(
                        methods
                            .iter()
                            .any(|method| method == request.method().as_str()),
                    ),
                    ForwardAccessMatcher::SourceCidrs { cidrs } => {
                        Some(source_cidrs_match(cidrs, client_addr))
                    }
                    ForwardAccessMatcher::DestinationPorts { ranges } => Some(
                        ranges
                            .iter()
                            .any(|range| (range.start..=range.end).contains(&destination.port)),
                    ),
                    ForwardAccessMatcher::Authenticated | ForwardAccessMatcher::Manager => {
                        Some(false)
                    }
                    ForwardAccessMatcher::DestinationLocal
                    | ForwardAccessMatcher::DestinationLinkLocal => None,
                };
                match value {
                    Some(value) if value == condition.negated => {
                        matched = false;
                        break;
                    }
                    Some(_) => {}
                    None => address_dependent = true,
                }
            }
            if !matched {
                continue;
            }
            if address_dependent {
                if rule.action == ForwardAccessAction::Allow {
                    return true;
                }
                continue;
            }
            return rule.action == ForwardAccessAction::Allow;
        }
        policy.default_action == ForwardAccessAction::Allow
    }

    fn condition_matches<B>(
        &self,
        condition: &ForwardAccessCondition,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
        addresses: &[IpAddr],
        principal: Option<&Principal>,
    ) -> bool {
        match &condition.matcher {
            ForwardAccessMatcher::All => true,
            ForwardAccessMatcher::Methods { methods } => methods
                .iter()
                .any(|method| method == request.method().as_str()),
            ForwardAccessMatcher::SourceCidrs { cidrs } => source_cidrs_match(cidrs, client_addr),
            ForwardAccessMatcher::DestinationPorts { ranges } => ranges
                .iter()
                .any(|range| (range.start..=range.end).contains(&destination.port)),
            ForwardAccessMatcher::Authenticated => principal.is_some(),
            ForwardAccessMatcher::DestinationLocal => addresses
                .iter()
                .any(|address| address.is_loopback() || self.local_addresses.contains(address)),
            ForwardAccessMatcher::DestinationLinkLocal => {
                addresses.iter().copied().any(is_link_local)
            }
            ForwardAccessMatcher::Manager => false,
        }
    }

    async fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, RequestFailure> {
        let name = header::PROXY_AUTHORIZATION;
        let credentials = match single_header(headers, &name) {
            HeaderCardinality::Single(value) => value.as_bytes(),
            HeaderCardinality::Missing | HeaderCardinality::Duplicate => {
                return Err(RequestFailure::Authentication);
            }
        };
        match self.auth.as_ref() {
            Some(ForwardAuthPlan::Bearer(token)) if token.authorizes(credentials) => {
                Ok(Principal::new("bearer"))
            }
            Some(ForwardAuthPlan::Basic(auth)) => auth
                .authenticate(credentials)
                .await
                .map(Principal::new)
                .ok_or(RequestFailure::Authentication),
            _ => Err(RequestFailure::Authentication),
        }
    }

    async fn connect_tcp_until(
        &self,
        addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<TcpStream, RequestFailure> {
        for address in addresses {
            match timeout_at(deadline, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(_)) => {}
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            }
        }
        Err(RequestFailure::BadGateway)
    }

    async fn connect_tunnel_until(
        &self,
        destination: &Destination,
        direct_addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let mut last_error = RequestFailure::BadGateway;
        if self.peer_direct_fallback != ForwardDirectFallback::Required {
            let attempts = self
                .peers
                .len()
                .min(self.peer_max_retries.saturating_add(1));
            for peer in self.peers.iter().take(attempts) {
                match self.connect_peer_tunnel(peer, destination, deadline).await {
                    Ok(stream) => return Ok(stream),
                    Err(error) => last_error = error,
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            if self.peer_direct_fallback == ForwardDirectFallback::Denied {
                return Err(last_error);
            }
        }
        self.connect_tcp_until(direct_addresses, deadline)
            .await
            .map(|stream| Box::new(stream) as BoxedIo)
    }

    async fn connect_peer_tunnel(
        &self,
        peer: &StaticPeerPlan,
        destination: &Destination,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let stream = self.connect_peer_tcp(peer, deadline).await?;
        self.connect_through_peer(stream, destination, deadline).await
    }

    async fn connect_peer_tcp(
        &self,
        peer: &StaticPeerPlan,
        deadline: Instant,
    ) -> Result<TcpStream, RequestFailure> {
        let peer_destination = Destination {
            host: peer.host.clone(),
            port: peer.port,
        };
        let addresses = self.resolve(&peer_destination).await?;
        let socket_addresses = addresses
            .into_iter()
            .map(|address| SocketAddr::new(address, peer.port))
            .collect::<Vec<_>>();
        self.connect_tcp_until(&socket_addresses, deadline).await
    }

    async fn connect_through_peer(
        &self,
        stream: TcpStream,
        destination: &Destination,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let (mut sender, connection) = timeout_at(
            deadline,
            hyper::client::conn::http1::handshake(TokioIo::new(stream)),
        )
        .await
        .map_err(|_| RequestFailure::GatewayTimeout)?
        .map_err(|_| RequestFailure::BadGateway)?;
        tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        });
        let request = Request::builder()
            .method(Method::CONNECT)
            .uri(destination.authority())
            .header(header::HOST, destination.authority())
            .body(Full::new(Bytes::new()))
            .map_err(|_| RequestFailure::BadRequest)?;
        let response = timeout_at(deadline, sender.send_request(request))
            .await
            .map_err(|_| RequestFailure::GatewayTimeout)?
            .map_err(|_| RequestFailure::BadGateway)?;
        if response.status() != StatusCode::OK {
            return Err(RequestFailure::BadGateway);
        }
        let upgraded = timeout_at(deadline, hyper::upgrade::on(response))
            .await
            .map_err(|_| RequestFailure::GatewayTimeout)?
            .map_err(|_| RequestFailure::BadGateway)?;
        Ok(Box::new(TokioIo::new(upgraded)))
    }

    async fn connect_http_with_peers(
        &self,
        destination: &Destination,
        scheme: ForwardScheme,
        direct_addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<ConnectedHttp, RequestFailure> {
        let mut last_error = RequestFailure::BadGateway;
        if self.peer_direct_fallback != ForwardDirectFallback::Required {
            let attempts = self
                .peers
                .len()
                .min(self.peer_max_retries.saturating_add(1));
            for peer in self.peers.iter().take(attempts) {
                match self
                    .connect_http_through_peer(peer, destination, scheme, deadline)
                    .await
                {
                    Ok(stream) => {
                        return Ok(ConnectedHttp {
                            stream,
                            via_peer: true,
                        });
                    }
                    Err(error) => last_error = error,
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            if self.peer_direct_fallback == ForwardDirectFallback::Denied {
                return Err(last_error);
            }
        }
        self.connect_http(destination, scheme, direct_addresses, deadline)
            .await
            .map(|stream| ConnectedHttp {
                stream,
                via_peer: false,
            })
    }

    async fn connect_http_through_peer(
        &self,
        peer: &StaticPeerPlan,
        destination: &Destination,
        scheme: ForwardScheme,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let stream = self.connect_peer_tcp(peer, deadline).await?;
        if scheme == ForwardScheme::Http {
            return Ok(Box::new(stream));
        }
        let tunnel = self.connect_through_peer(stream, destination, deadline).await?;
        self.connect_tls_stream(destination, tunnel, deadline).await
    }

    async fn connect_http(
        &self,
        destination: &Destination,
        scheme: ForwardScheme,
        addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let stream = self.connect_tcp_until(addresses, deadline).await?;
        if scheme == ForwardScheme::Http {
            return Ok(Box::new(stream));
        }
        self.connect_tls_stream(destination, Box::new(stream), deadline)
            .await
    }

    async fn connect_tls_stream(
        &self,
        destination: &Destination,
        stream: BoxedIo,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let tls_identity = match &destination.host {
            Host::Dns(host) => host.clone(),
            Host::Ip(address) => address.to_string(),
        };
        let ssl = self
            .tls_connector
            .configure()
            .and_then(|configuration| configuration.into_ssl(&tls_identity))
            .map_err(|_| RequestFailure::BadGateway)?;
        let mut stream = SslStream::new(ssl, stream).map_err(|_| RequestFailure::BadGateway)?;
        timeout_at(deadline, Pin::new(&mut stream).connect())
            .await
            .map_err(|_| RequestFailure::GatewayTimeout)?
            .map_err(|_| RequestFailure::BadGateway)?;
        Ok(Box::new(stream))
    }

    fn rejection(&self, failure: RequestFailure) -> Response<ForwardProxyBody> {
        let mut response = response(failure.status(), Bytes::new());
        if failure == RequestFailure::Authentication {
            if let Some(challenge) = &self.challenge {
                response
                    .headers_mut()
                    .insert(header::PROXY_AUTHENTICATE, challenge.clone());
            }
        }
        response
    }

    async fn reject_h2(&self, session: &mut ServerSession, failure: RequestFailure) {
        let Ok(mut response_header) = pingora::http::ResponseHeader::build(failure.status(), None)
        else {
            session.shutdown().await;
            return;
        };
        if failure == RequestFailure::Authentication {
            if let Some(challenge) = &self.challenge {
                let _ =
                    response_header.insert_header(header::PROXY_AUTHENTICATE, challenge.clone());
            }
        }
        if session
            .write_response_header(Box::new(response_header))
            .await
            .is_err()
        {
            return;
        }
        let _ = session.finish_body().await;
    }
}

async fn recv_h3_body<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    limit: usize,
    deadline: Instant,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Bytes, RequestFailure>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let mut body = BytesMut::new();
    loop {
        let chunk = tokio::select! {
            result = timeout_at(deadline, stream.recv_data()) => {
                result.map_err(|_| RequestFailure::GatewayTimeout)?
                    .map_err(|_| RequestFailure::BadRequest)?
            }
            _ = shutdown.changed() => return Err(RequestFailure::GatewayTimeout),
        };
        let Some(mut chunk) = chunk else {
            return Ok(body.freeze());
        };
        if chunk.remaining() > limit.saturating_sub(body.len()) {
            return Err(RequestFailure::PayloadTooLarge);
        }
        let bytes = chunk.copy_to_bytes(chunk.remaining());
        body.extend_from_slice(&bytes);
    }
}

async fn send_h3_failure<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    failure: RequestFailure,
    challenge: Option<&HeaderValue>,
) -> Result<(), h3::error::StreamError>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let mut response = Response::new(());
    *response.status_mut() = failure.status();
    if failure == RequestFailure::Authentication {
        if let Some(challenge) = challenge {
            response
                .headers_mut()
                .insert(header::PROXY_AUTHENTICATE, challenge.clone());
        }
    }
    stream.send_response(response).await?;
    stream.finish().await
}

#[async_trait]
impl HttpServerApp for ForwardHttp1ServicePlan {
    #[allow(clippy::too_many_lines)]
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        if !session.is_http2() {
            // This plan is selected by a ForwardHttp2 listener. Never reinterpret H1 as H2.
            session.shutdown().await;
            return None;
        }
        if session.req_header().method != Method::CONNECT {
            self.reject_h2(&mut session, RequestFailure::BadRequest)
                .await;
            return None;
        }
        let Ok(request) = h2_request(&session) else {
            self.reject_h2(&mut session, RequestFailure::BadRequest)
                .await;
            return None;
        };
        let authorized = match self
            .authorize_request(
                &request,
                Protocol::Http2,
                session
                    .client_addr()
                    .and_then(|address| address.as_inet().copied()),
            )
            .await
        {
            Ok(request) => request,
            Err(error) => {
                self.reject_h2(&mut session, error).await;
                return None;
            }
        };
        let AuthorizedRequest {
            approved,
            parsed: ParsedTarget::Tunnel(destination),
            lifetime_deadline,
            ..
        } = authorized
        else {
            self.reject_h2(&mut session, RequestFailure::BadRequest)
                .await;
            return None;
        };
        let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
        let upstream = match timeout_at(
            lifetime_deadline,
            self.connect_tcp_until(approved.socket_addresses.as_ref(), connect_deadline),
        )
        .await
        {
            Ok(Ok(upstream)) => upstream,
            Ok(Err(error)) => {
                self.reject_h2(&mut session, error).await;
                return None;
            }
            Err(_) => {
                self.reject_h2(&mut session, RequestFailure::GatewayTimeout)
                    .await;
                return None;
            }
        };
        let Ok(response_header) = pingora::http::ResponseHeader::build(StatusCode::OK, None) else {
            self.reject_h2(&mut session, RequestFailure::BadGateway)
                .await;
            return None;
        };
        if session
            .write_response_header(Box::new(response_header))
            .await
            .is_err()
        {
            return None;
        }
        let lifetime_timeout = lifetime_deadline.saturating_duration_since(Instant::now());
        if lifetime_timeout.is_zero() {
            session.shutdown().await;
            return None;
        }
        let Ok(tunnel) = BoundedTunnel::new(TunnelLimits {
            idle_timeout: self.idle_timeout,
            lifetime_timeout,
            ..TunnelLimits::default()
        }) else {
            session.shutdown().await;
            return None;
        };
        let mut shutdown = shutdown.clone();
        let outcome = tokio::select! {
            outcome = tunnel.relay_h2(
                PingoraH2Stream { session },
                upstream,
            ) => Some(outcome),
            _ = shutdown.changed() => None,
        };
        match outcome.as_ref() {
            Some(outcome) => log::info!(
                target: "oxiroute::forward_proxy",
                "event=forward_tunnel protocol=h2 destination={} outcome={} bytes_left_to_right={} bytes_right_to_left={}",
                destination.authority(),
                outcome.kind().as_str(),
                outcome.stats().left_to_right,
                outcome.stats().right_to_left,
            ),
            None => log::info!(
                target: "oxiroute::forward_proxy",
                "event=forward_tunnel protocol=h2 destination={} outcome=cancelled bytes_left_to_right=0 bytes_right_to_left=0",
                destination.authority(),
            ),
        }
        None
    }

    fn h2_options(&self) -> Option<H2Options> {
        let mut options = default_h2_options();
        options.max_header_list_size(u32::try_from(self.max_header_bytes).unwrap_or(u32::MAX));
        Some(options)
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        Some(&self.http_server_options)
    }
}

struct PingoraH2Stream {
    session: ServerSession,
}

#[async_trait]
impl H2TunnelStream for PingoraH2Stream {
    async fn recv_data(&mut self) -> io::Result<Option<Bytes>> {
        let data = self
            .session
            .read_request_body()
            .await
            .map_err(|error| pingora_io_error(&error))?;
        if data
            .as_ref()
            .is_some_and(|data| data.is_empty() && self.session.is_body_done())
        {
            Ok(None)
        } else {
            Ok(data)
        }
    }

    async fn send_data(&mut self, data: Bytes, end: bool) -> io::Result<()> {
        self.session
            .write_response_body(data, end)
            .await
            .map_err(|error| pingora_io_error(&error))
    }

    async fn wait_closed(&mut self) -> io::Result<()> {
        self.session
            .read_body_or_idle(true)
            .await
            .map(|_| ())
            .map_err(|error| pingora_io_error(&error))
    }

    async fn reset(&mut self) {
        self.session.shutdown().await;
    }
}

fn pingora_io_error(error: &pingora::Error) -> io::Error {
    io::Error::other(error.to_string())
}

fn h2_request(session: &ServerSession) -> Result<Request<()>, ()> {
    let header = session.req_header();
    let mut request = Request::builder()
        .method(header.method.clone())
        .uri(header.uri.clone())
        .body(())
        .map_err(|_| ())?;
    *request.headers_mut() = header.headers.clone();
    Ok(request)
}

fn resolver(service: &ForwardProxyService) -> Result<TokioAsyncResolver, ForwardPlanError> {
    let mut options = ResolverOpts::default();
    options.ip_strategy = LookupIpStrategy::Ipv4thenIpv6;
    options.cache_size =
        usize::try_from(service.resolver.max_cache_entries).map_err(|_| ForwardPlanError::Limit)?;
    options.positive_min_ttl = Some(Duration::from_millis(service.resolver.min_ttl_ms));
    options.positive_max_ttl = Some(Duration::from_millis(service.resolver.max_ttl_ms));
    options.negative_min_ttl = Some(Duration::from_millis(service.resolver.negative_ttl_ms));
    options.negative_max_ttl = Some(Duration::from_millis(service.resolver.negative_ttl_ms));
    if service.resolver.nameservers.is_empty() {
        let (config, _) = hickory_resolver::system_conf::read_system_conf()
            .map_err(|_| ForwardPlanError::Resolver)?;
        Ok(TokioAsyncResolver::tokio(config, options))
    } else {
        let nameservers =
            NameServerConfigGroup::from_ips_clear(&service.resolver.nameservers, 53, true);
        Ok(TokioAsyncResolver::tokio(
            ResolverConfig::from_parts(None, Vec::new(), nameservers),
            options,
        ))
    }
}

fn static_peer_plan(peer: &ForwardPeer) -> Result<StaticPeerPlan, ForwardPlanError> {
    if peer.host.is_empty() || peer.port == 0 {
        return Err(ForwardPlanError::Peer);
    }
    let host = peer
        .host
        .parse::<IpAddr>()
        .map_or_else(|_| Host::Dns(peer.host.clone()), Host::Ip);
    Ok(StaticPeerPlan {
        host,
        port: peer.port,
    })
}

fn absolute_form(target: &oxiroute_forward_proxy::ForwardTarget) -> Option<Uri> {
    let path = target
        .origin_form
        .path_and_query()
        .map_or(target.origin_form.path(), |value| value.as_str());
    Uri::builder()
        .scheme(target.scheme.as_str())
        .authority(target.destination.authority())
        .path_and_query(path)
        .build()
        .ok()
}

fn destination_time_windows(ranges: &[ForwardTimeRange]) -> Result<Vec<TimeWindow>, RuleError> {
    ranges
        .iter()
        .map(|range| {
            let days = range.days.iter().fold(0_u8, |days, day| {
                days | match day {
                    ForwardWeekday::Monday => 1 << 0,
                    ForwardWeekday::Tuesday => 1 << 1,
                    ForwardWeekday::Wednesday => 1 << 2,
                    ForwardWeekday::Thursday => 1 << 3,
                    ForwardWeekday::Friday => 1 << 4,
                    ForwardWeekday::Saturday => 1 << 5,
                    ForwardWeekday::Sunday => 1 << 6,
                }
            });
            TimeWindow::new(
                days,
                parse_forward_time(&range.start).ok_or(RuleError::InvalidTimeWindow)?,
                parse_forward_time(&range.end).ok_or(RuleError::InvalidTimeWindow)?,
            )
        })
        .collect()
}

fn parse_forward_time(value: &str) -> Option<u16> {
    if value.len() != 5 || !value.is_ascii() || value.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let hour = value[..2].parse::<u16>().ok()?;
    let minute = value[3..].parse::<u16>().ok()?;
    if hour > 24 || minute > 59 || (hour == 24 && minute != 0) {
        return None;
    }
    Some(hour * 60 + minute)
}

fn apply_header_policy(headers: &mut HeaderMap, policy: &ForwardHeaderPolicy) {
    if policy.forwarded_for == ForwardedForPolicy::Delete {
        headers.remove(header::FORWARDED);
        headers.remove("x-forwarded-for");
    }
    if policy.via == ForwardViaPolicy::Delete {
        headers.remove(header::VIA);
    }
}

fn sanitize_response_headers(headers: &mut HeaderMap) -> Result<(), ()> {
    let mut nominated = Vec::new();
    for value in headers.get_all(header::CONNECTION) {
        let value = value.to_str().map_err(|_| ())?;
        for name in value.split(',') {
            nominated.push(name.trim().parse::<http::HeaderName>().map_err(|_| ())?);
        }
    }
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION,
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
    Ok(())
}

fn response(status: StatusCode, body: Bytes) -> Response<ForwardProxyBody> {
    Response::builder()
        .status(status)
        .body(
            Full::new(body)
                .map_err(|never: Infallible| -> BoxError { match never {} })
                .boxed(),
        )
        .expect("static forward response")
}

fn cidr_contains(network: &str, address: IpAddr) -> bool {
    let Some((base, prefix)) = network.split_once('/') else {
        return false;
    };
    let (Ok(base), Ok(prefix)) = (base.parse::<IpAddr>(), prefix.parse::<u8>()) else {
        return false;
    };
    match (base, address) {
        (IpAddr::V4(base), IpAddr::V4(address)) if prefix <= 32 => {
            let host_bits = 32 - u32::from(prefix);
            let mask = if host_bits == 32 {
                0
            } else {
                u32::MAX << host_bits
            };
            u32::from(base) & mask == u32::from(address) & mask
        }
        (IpAddr::V6(base), IpAddr::V6(address)) if prefix <= 128 => {
            let host_bits = 128 - u32::from(prefix);
            let mask = if host_bits == 128 {
                0
            } else {
                u128::MAX << host_bits
            };
            u128::from(base) & mask == u128::from(address) & mask
        }
        _ => false,
    }
}

fn source_cidrs_match(cidrs: &[String], client_addr: Option<SocketAddr>) -> bool {
    client_addr.is_some_and(|client_addr| {
        let address = match client_addr.ip() {
            IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .map_or(IpAddr::V6(address), IpAddr::V4),
            IpAddr::V4(address) => IpAddr::V4(address),
        };
        cidrs.iter().any(|network| cidr_contains(network, address))
    })
}

const fn is_link_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_link_local(),
        IpAddr::V6(address) => (address.segments()[0] & 0xffc0) == 0xfe80,
    }
}

const fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http1 => "h1",
        Protocol::Http2 => "h2",
        Protocol::Http3 => "h3",
    }
}

#[cfg(test)]
mod tests {
    use super::{ForwardAccessMetrics, ForwardAccessResult, source_cidrs_match};

    #[test]
    fn missing_inet_peer_never_matches_source_cidrs() {
        assert!(!source_cidrs_match(&["0.0.0.0/0".into()], None));
    }

    #[test]
    fn ipv4_mapped_peer_matches_ipv4_source_cidr() {
        let client_addr = "[::ffff:127.0.0.1]:54321"
            .parse()
            .expect("mapped loopback address");

        assert!(source_cidrs_match(
            &["127.0.0.0/8".into()],
            Some(client_addr)
        ));
    }

    #[test]
    fn access_metrics_classify_results_without_retaining_request_data() {
        let metrics = ForwardAccessMetrics::default();
        metrics.record(ForwardAccessResult::Allowed);
        metrics.record(ForwardAccessResult::Forbidden);
        metrics.record(ForwardAccessResult::Forbidden);

        assert_eq!(
            metrics.snapshot(),
            super::ForwardAccessMetricsSnapshot {
                allowed: 1,
                forbidden: 2,
                ..super::ForwardAccessMetricsSnapshot::default()
            }
        );
    }
}
