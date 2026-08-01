use std::{
    collections::{HashSet, VecDeque},
    convert::Infallible,
    error::Error,
    future::{Future as _, poll_fn},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use hickory_resolver::{
    TokioAsyncResolver,
    config::{LookupIpStrategy, NameServerConfigGroup, ResolverConfig, ResolverOpts},
};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt as _, Full, Limited, combinators::BoxBody};
use hyper::body::{Body, Frame, Incoming};
use hyper_util::rt::TokioIo;
use openssl::{
    sha::Sha256,
    ssl::{SslConnector, SslMethod, SslVerifyMode},
};
use oxiroute_config::{
    ForwardAccessAction, ForwardAccessCondition, ForwardAccessMatcher, ForwardAccessPolicy,
    ForwardAuditMode, ForwardHeaderPolicy, ForwardProxyAuth, ForwardProxyService, ForwardViaPolicy,
    ForwardedForPolicy,
};
use oxiroute_forward_proxy::{
    BoundedTunnel, Destination, DestinationPolicy as _, DestinationRules, ForwardScheme, Host,
    PolicyContext, Principal, Protocol, TunnelLimits, parse_absolute_form, parse_connect_authority,
    sanitize_request_headers,
};
use pingora::server::ShutdownWatch;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time::{Instant, Sleep, timeout_at},
};
use tokio_openssl::SslStream;

use crate::{
    http_action::BasicHtpasswdAccess,
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
};

type BoxError = Box<dyn Error + Send + Sync>;
pub type ForwardProxyBody = BoxBody<Bytes, BoxError>;
const MAX_BASIC_CREDENTIAL_CACHE_ENTRIES: usize = 4_096;

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
    header_policy: ForwardHeaderPolicy,
    idle_timeout: Duration,
    lifetime_timeout: Duration,
    local_addresses: Arc<HashSet<IpAddr>>,
    max_header_bytes: usize,
    max_request_body_bytes: usize,
    name: String,
    resolver: TokioAsyncResolver,
    resolver_addresses: usize,
    resolver_queries: Arc<Semaphore>,
    service_connections: Arc<Semaphore>,
    tls_connector: Arc<SslConnector>,
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

enum ParsedTarget {
    Forward(oxiroute_forward_proxy::ForwardTarget),
    Tunnel(Destination),
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

impl ForwardHttp1ServicePlan {
    pub(crate) fn compile(service: &ForwardProxyService) -> Result<Self, ForwardPlanError> {
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
        .map_err(|_| ForwardPlanError::DestinationPolicy)?;
        let resolver = resolver(service)?;
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
        let resolver_addresses = usize::try_from(service.resolver.max_addresses_per_name)
            .map_err(|_| ForwardPlanError::Limit)?;
        let resolver_queries = usize::try_from(service.resolver.max_concurrent_queries)
            .map_err(|_| ForwardPlanError::Limit)?;
        let local_addresses = local_ip_address::list_afinet_netifas()
            .map_err(|_| ForwardPlanError::Resolver)?
            .into_iter()
            .map(|(_, address)| address)
            .collect();

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
            header_policy: service.header_policy,
            idle_timeout: Duration::from_millis(service.idle_timeout_ms),
            lifetime_timeout: Duration::from_millis(service.lifetime_timeout_ms),
            local_addresses: Arc::new(local_addresses),
            max_header_bytes,
            max_request_body_bytes,
            name: service.name.clone(),
            resolver,
            resolver_addresses,
            resolver_queries: Arc::new(Semaphore::new(resolver_queries)),
            service_connections: Arc::new(Semaphore::new(max_connections)),
            tls_connector: Arc::new(connector.build()),
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

    #[allow(clippy::too_many_lines)]
    pub async fn handle(
        self: &Arc<Self>,
        mut request: Request<Incoming>,
        client_addr: Option<SocketAddr>,
        mut shutdown: ShutdownWatch,
        lifecycle: Arc<ForwardConnectionLifecycle>,
    ) -> Response<ForwardProxyBody> {
        let target = request.uri().to_string();
        let parsed = if request.method() == Method::CONNECT {
            parse_connect_authority(&target).map(ParsedTarget::Tunnel)
        } else if self.allow_absolute_form {
            parse_absolute_form(&target).map(ParsedTarget::Forward)
        } else {
            return self.rejection(RequestFailure::Forbidden);
        };
        let Ok(parsed) = parsed else {
            return self.rejection(RequestFailure::BadRequest);
        };
        let destination = match &parsed {
            ParsedTarget::Forward(target) => &target.destination,
            ParsedTarget::Tunnel(destination) => destination,
        };
        let lifetime_deadline = Instant::now() + self.lifetime_timeout;
        if matches!(parsed, ParsedTarget::Tunnel(_))
            && (!self.connect_enabled || !self.connect_ports.contains(&destination.port))
        {
            return self.rejection(RequestFailure::Forbidden);
        }
        if self.pre_resolution_denied(&request, client_addr, destination) {
            return self.rejection(RequestFailure::Forbidden);
        }
        let principal = if self.auth.is_some()
            && !self.anonymous_access_possible(&request, client_addr, destination)
        {
            match timeout_at(lifetime_deadline, self.authenticate(request.headers())).await {
                Ok(Ok(principal)) => Some(principal),
                Ok(Err(error)) => return self.rejection(error),
                Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
            }
        } else {
            None
        };
        let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
        let addresses = match timeout_at(connect_deadline, self.resolve(destination)).await {
            Ok(Ok(addresses)) => addresses,
            Ok(Err(error)) => return self.rejection(error),
            Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
        };
        let principal = match timeout_at(
            lifetime_deadline,
            self.authorize_access(&request, client_addr, destination, &addresses, principal),
        )
        .await
        {
            Ok(Ok(principal)) => principal,
            Ok(Err(error)) => return self.rejection(error),
            Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
        };
        let policy_principal = principal
            .clone()
            .unwrap_or_else(|| Principal::new("anonymous"));
        if self
            .destination_policy
            .authorize(
                &PolicyContext {
                    protocol: Protocol::Http1,
                    principal: policy_principal,
                },
                destination,
                &addresses,
            )
            .is_err()
        {
            return self.rejection(RequestFailure::Forbidden);
        }
        if self.audit_mode == ForwardAuditMode::Metadata {
            log::info!(
                target: "oxiroute::forward_proxy",
                "service={} method={} destination={} authenticated={}",
                self.name,
                request.method(),
                destination.authority(),
                principal.is_some()
            );
        }
        let socket_addresses = addresses
            .iter()
            .map(|address| SocketAddr::new(*address, destination.port))
            .collect::<Vec<_>>();

        match parsed {
            ParsedTarget::Tunnel(_) => {
                let upstream = match timeout_at(
                    lifetime_deadline,
                    self.connect_tcp_until(&socket_addresses, connect_deadline),
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
                        log::debug!(
                            target: "oxiroute::forward_proxy",
                            "CONNECT tunnel ended: {outcome:?}"
                        );
                    }
                });
                response(StatusCode::OK, Bytes::new())
            }
            ParsedTarget::Forward(target) => {
                if request.body().size_hint().upper().is_some_and(|length| {
                    length > u64::try_from(self.max_request_body_bytes).unwrap_or(u64::MAX)
                }) {
                    return self.rejection(RequestFailure::PayloadTooLarge);
                }
                let upstream = match timeout_at(
                    lifetime_deadline,
                    self.connect_http(
                        &target.destination,
                        target.scheme,
                        &socket_addresses,
                        connect_deadline,
                    ),
                )
                .await
                {
                    Ok(Ok(upstream)) => upstream,
                    Ok(Err(error)) => return self.rejection(error),
                    Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
                };
                *request.uri_mut() = target.origin_form;
                let Ok(mut headers) =
                    sanitize_request_headers(request.headers(), &target.destination)
                else {
                    return self.rejection(RequestFailure::BadRequest);
                };
                apply_header_policy(&mut headers, self.header_policy);
                *request.headers_mut() = headers;
                let (parts, body) = request.into_parts();
                let (body, mut body_completion) = relay_request_body(
                    body,
                    self.max_request_body_bytes,
                    self.idle_timeout,
                    lifetime_deadline,
                );
                let request = Request::from_parts(parts, body);
                let handshake_deadline =
                    lifetime_deadline.min(Instant::now() + self.connect_timeout);
                let (mut sender, connection) = match timeout_at(
                    handshake_deadline,
                    hyper::client::conn::http1::handshake(TokioIo::new(upstream)),
                )
                .await
                {
                    Ok(Ok(connection)) => connection,
                    Ok(Err(_)) => return self.rejection(RequestFailure::BadGateway),
                    Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
                };
                tokio::spawn(async move {
                    let _ = connection.await;
                });
                let response = sender.send_request(request);
                tokio::pin!(response);
                let mut body_complete = false;
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
                                Ok(Err(error)) => return self.rejection(error),
                                Err(_) => return self.rejection(RequestFailure::BadGateway),
                            }
                        }
                        result = &mut response => break match result {
                            Ok(response) => response,
                            Err(_) => return self.rejection(RequestFailure::BadGateway),
                        },
                        () = &mut response_idle, if body_complete => {
                            return self.rejection(RequestFailure::GatewayTimeout);
                        }
                        () = tokio::time::sleep_until(lifetime_deadline) => {
                            return self.rejection(RequestFailure::GatewayTimeout);
                        }
                    }
                };
                if sanitize_response_headers(upstream_response.headers_mut()).is_err() {
                    return self.rejection(RequestFailure::BadGateway);
                }
                if !body_complete {
                    upstream_response
                        .headers_mut()
                        .insert(header::CONNECTION, HeaderValue::from_static("close"));
                    drop(body_completion);
                }
                upstream_response
                    .map(|body| TimedBody::new(body, self.idle_timeout, lifetime_deadline).boxed())
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

    async fn authorize_access(
        &self,
        request: &Request<Incoming>,
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

    fn pre_resolution_denied(
        &self,
        request: &Request<Incoming>,
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

    fn anonymous_access_possible(
        &self,
        request: &Request<Incoming>,
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

    fn condition_matches(
        &self,
        condition: &ForwardAccessCondition,
        request: &Request<Incoming>,
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

fn apply_header_policy(headers: &mut HeaderMap, policy: ForwardHeaderPolicy) {
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

#[cfg(test)]
mod tests {
    use super::source_cidrs_match;

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
}
