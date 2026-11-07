#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) type BoxError = Box<dyn Error + Send + Sync>;
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
    pub(super) fn start(self: &Arc<Self>) -> ForwardTunnelCompletion {
        self.started.store(true, Ordering::Release);
        ForwardTunnelCompletion(Arc::clone(self))
    }

    pub async fn wait_if_started(&self) {
        if self.started.load(Ordering::Acquire) {
            self.finished.notified().await;
        }
    }
}

pub(super) struct ForwardTunnelCompletion(Arc<ForwardConnectionLifecycle>);

impl Drop for ForwardTunnelCompletion {
    fn drop(&mut self) {
        self.0.finished.notify_one();
    }
}

pub(super) trait ProxyIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ProxyIo for T {}
pub(super) type BoxedIo = Box<dyn ProxyIo>;

pub(super) enum ForwardAuthPlan {
    Bearer(SecureBearerToken),
    Basic(Box<ForwardBasicAuth>),
}

pub(super) struct ForwardBasicAuth {
    access: Arc<BasicHtpasswdAccess>,
    cache: Mutex<VecDeque<CachedBasicCredential>>,
    cache_salt: [u8; 32],
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
    pub(super) fn new(
        access: BasicHtpasswdAccess,
        path: PathBuf,
        realm: String,
        ttl: Option<Duration>,
        username_case_sensitive: bool,
    ) -> Result<Self, ForwardPlanError> {
        let mut cache_salt = [0; 32];
        openssl::rand::rand_bytes(&mut cache_salt).map_err(|_| ForwardPlanError::Authentication)?;
        Ok(Self {
            access: Arc::new(access),
            cache: Mutex::new(VecDeque::new()),
            cache_salt,
            path,
            realm,
            refresh: Mutex::new(()),
            ttl,
            username_case_sensitive,
        })
    }

    pub(super) async fn authenticate(&self, credentials: &[u8]) -> Option<Arc<str>> {
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

// These flags represent independent listener policies; combining them would obscure their wire semantics.
#[allow(clippy::struct_excessive_bools)]
pub struct ForwardHttp1ServicePlan {
    pub(super) access_policy: Option<ForwardAccessPolicy>,
    pub(super) allow_absolute_form: bool,
    pub(super) audit_mode: ForwardAuditMode,
    pub(super) auth: Option<ForwardAuthPlan>,
    pub(super) challenge: Option<HeaderValue>,
    pub(super) connect_enabled: bool,
    pub(super) connect_ports: Arc<[u16]>,
    pub(super) connect_udp_enabled: bool,
    pub(super) connect_udp_ports: Arc<[u16]>,
    pub(super) connect_timeout: Duration,
    pub(super) destination_policy: DestinationRules,
    pub(super) peer_direct_fallback: ForwardDirectFallback,
    pub(super) peer_max_retries: usize,
    pub(super) peers: Arc<[StaticPeerPlan]>,
    pub(super) header_policy: ForwardHeaderPolicy,
    pub(super) http_server_options: HttpServerOptions,
    pub(super) idle_timeout: Duration,
    pub(super) lifetime_timeout: Duration,
    pub(super) local_addresses: Arc<HashSet<IpAddr>>,
    pub(super) max_header_bytes: usize,
    pub(super) max_request_body_bytes: usize,
    pub(super) name: String,
    pub(super) resolver: TokioResolver,
    pub(super) resolver_addresses: usize,
    pub(super) resolver_revalidate_on_connect: bool,
    pub(super) resolver_queries: Arc<Semaphore>,
    pub(super) service_connections: Arc<Semaphore>,
    pub(super) tls_connector: Arc<SslConnector>,
    pub(super) access_metrics: Arc<ForwardAccessMetrics>,
    pub(super) cache: Option<Arc<HttpCachePlan>>,
    pub(super) h3_upstream: Option<Arc<H3UpstreamPlan>>,
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
pub(super) enum RequestFailure {
    BadRequest,
    Authentication,
    Forbidden,
    BadGateway,
    GatewayTimeout,
    PayloadTooLarge,
}

#[derive(Clone, Copy)]
pub(super) enum ForwardCacheFailure {
    Local,
    UpstreamConnect,
    UpstreamTimeout,
    UpstreamProtocol,
    UpstreamServerStatus,
}

impl ForwardCacheFailure {
    pub(super) const fn cache_class(self) -> CacheFailureClass {
        match self {
            Self::Local => CacheFailureClass::Local,
            Self::UpstreamConnect
            | Self::UpstreamTimeout
            | Self::UpstreamProtocol
            | Self::UpstreamServerStatus => CacheFailureClass::Upstream,
        }
    }
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
pub(super) struct ForwardAccessMetrics {
    counts: [AtomicU64; ForwardAccessResult::ALL.len()],
}

impl ForwardAccessMetrics {
    pub(super) fn record(&self, result: ForwardAccessResult) {
        let counter = &self.counts[result.index()];
        let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        });
    }

    pub(super) fn snapshot(&self) -> ForwardAccessMetricsSnapshot {
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
    pub(super) const fn access_result(self) -> ForwardAccessResult {
        match self {
            Self::BadRequest => ForwardAccessResult::BadRequest,
            Self::Authentication => ForwardAccessResult::Authentication,
            Self::Forbidden => ForwardAccessResult::Forbidden,
            Self::BadGateway => ForwardAccessResult::BadGateway,
            Self::GatewayTimeout => ForwardAccessResult::Timeout,
            Self::PayloadTooLarge => ForwardAccessResult::PayloadTooLarge,
        }
    }

    pub(super) const fn reason(self) -> &'static str {
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
    pub(super) const fn status(self) -> StatusCode {
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
pub(super) enum ForwardCacheDecision {
    Bypass,
    Respond(Response<ForwardProxyBody>),
    Continue(CacheTransaction),
}

pub(super) enum H3ForwardCacheDecision {
    Bypass,
    Respond(Box<CachedResponse>),
    OnlyIfCached,
    Continue(Box<CacheTransaction>),
}

pub(super) enum ParsedTarget {
    Forward(oxiroute_forward_proxy::ForwardTarget),
    Tunnel(Destination),
    UdpTunnel(Destination),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConnectUdpRequestKind {
    NotAttempt,
    Valid,
    Malformed,
}

pub(super) struct AuthorizedRequest {
    pub(super) approved: ApprovedDestination,
    pub(super) authenticated: bool,
    pub(super) parsed: ParsedTarget,
    pub(super) lifetime_deadline: Instant,
}
