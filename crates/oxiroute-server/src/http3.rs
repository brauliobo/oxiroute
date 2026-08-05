use std::{
    error::Error,
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use bytes::{Buf as _, Bytes};
use h3::server::RequestResolver;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
    header::{CONNECTION, CONTENT_LENGTH, HOST, TE, TRAILER, TRANSFER_ENCODING, UPGRADE},
    uri::Authority,
};
use log::{error, warn};
use oxiroute_config::{DownstreamTimeoutPolicy, HttpRedirectLocation, is_unambiguous_http_path};
use pingora::{connectors::http::Connector, http::RequestHeader};
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Endpoint, EndpointConfig, ServerConfig, TransportConfig, VarInt};
use rustls::server::{ClientHello, ResolvesServerCert};
use tokio::{
    runtime::Builder,
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    time::{Instant, timeout_at},
};

use crate::{
    ForwardHttp1ServicePlan, HttpOperationResult, HttpServicePlan, ListenerMetrics,
    ListenerReservation, ListenerRuntimeState, RuntimeGeneration, RuntimeReferenceKind,
    TlsProfilePlan,
    http_action::{
        HttpActionPlan, ProxyPolicyPlan, RequestHeaderMutationPlan, RequestHeaderValuePlan,
    },
    http_proxy::{
        apply_response_policy, remove_upstream_hop_by_hop_response_headers, rewrite_upstream_path,
        selected_upstream_host,
    },
    upstream_peer::validate_tls_connection,
};

pub(crate) const H3_HANDSHAKE_LIMIT: usize = 64;
pub(crate) const H3_BIDI_STREAM_LIMIT: u32 = 128;
pub(crate) const H3_UNI_STREAM_LIMIT: u32 = 16;
const H3_STREAM_RECEIVE_WINDOW: u32 = 1024 * 1024;
const H3_CONNECTION_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;
const H3_INCOMING_BUFFER: u64 = 1024 * 1024;
const H3_TOTAL_INCOMING_BUFFER: u64 = 16 * 1024 * 1024;
pub(crate) const H3_MAX_FIELD_SECTION_BYTES: u64 = 16 * 1024;
pub(crate) const H3_MAX_RESPONSE_BODY_BYTES: u64 = 64 * 1024 * 1024;
const H3_CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const H3_CLOSE_CODE: VarInt = VarInt::from_u32(0);
const KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
const PROXY_CONNECTION: HeaderName = HeaderName::from_static("proxy-connection");

pub struct Http3Runtime {
    thread: Option<JoinHandle<()>>,
}

impl Http3Runtime {
    /// Starts one active forward HTTP/3 listener on a previously reserved UDP socket.
    ///
    /// # Errors
    ///
    /// Returns an error unless the QUIC endpoint, TLS configuration, and H3 accept loop all reach
    /// their startup boundary.
    pub fn start_forward(
        listener_name: String,
        reservation: ListenerReservation,
        service: Arc<ForwardHttp1ServicePlan>,
        tls: Arc<TlsProfilePlan>,
        generation: Arc<RuntimeGeneration>,
        metrics: ListenerMetrics,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self, Box<dyn Error>> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let startup_listener_name = listener_name.clone();
        let thread = thread::Builder::new()
            .name(format!("oxiroute-h3-{listener_name}"))
            .spawn(move || {
                let result = run_forward(
                    &listener_name,
                    reservation,
                    service,
                    tls,
                    generation.clone(),
                    metrics,
                    shutdown,
                    ready_tx,
                );
                if let Err(error) = result {
                    generation.mark_runtime_failed();
                    error!("HTTP/3 listener `{listener_name}` failed: {error}");
                }
            })?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(io::Error::other(error).into())
            }
            Err(error) => {
                let _ = thread.join();
                Err(io::Error::new(
                    if error == std::sync::mpsc::RecvTimeoutError::Timeout {
                        io::ErrorKind::TimedOut
                    } else {
                        io::ErrorKind::BrokenPipe
                    },
                    format!(
                        "HTTP/3 listener `{startup_listener_name}` did not become ready: {error}"
                    ),
                )
                .into())
            }
        }
    }

    /// Starts one active reverse HTTP/3 listener on a previously reserved UDP socket.
    ///
    /// The reverse listener owns QUIC directly because Pingora does not expose a downstream H3
    /// service app. Origin requests still use Pingora's immutable upstream pool and HTTP session
    /// plans; no H3 request is relabeled as an H1/H2 downstream response.
    ///
    /// # Errors
    ///
    /// Returns an error unless the QUIC endpoint, TLS configuration, and H3 accept loop all reach
    /// their startup boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn start_reverse(
        listener_name: String,
        reservation: ListenerReservation,
        service: Arc<HttpServicePlan>,
        tls: Arc<TlsProfilePlan>,
        generation: Arc<RuntimeGeneration>,
        metrics: ListenerMetrics,
        downstream_timeouts: DownstreamTimeoutPolicy,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self, Box<dyn Error>> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let startup_listener_name = listener_name.clone();
        let thread = thread::Builder::new()
            .name(format!("oxiroute-h3-reverse-{listener_name}"))
            .spawn(move || {
                let result = run_reverse(
                    &listener_name,
                    reservation,
                    service,
                    tls,
                    generation.clone(),
                    metrics,
                    downstream_timeouts,
                    shutdown,
                    ready_tx,
                );
                if let Err(error) = result {
                    generation.mark_runtime_failed();
                    error!("reverse HTTP/3 listener `{listener_name}` failed: {error}");
                }
            })?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(io::Error::other(error).into())
            }
            Err(error) => {
                let _ = thread.join();
                Err(io::Error::new(
                    if error == std::sync::mpsc::RecvTimeoutError::Timeout {
                        io::ErrorKind::TimedOut
                    } else {
                        io::ErrorKind::BrokenPipe
                    },
                    format!(
                        "reverse HTTP/3 listener `{startup_listener_name}` did not become ready: {error}"
                    ),
                )
                .into())
            }
        }
    }

    /// Waits for the listener and all active H3 connections to stop.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener thread panicked.
    ///
    /// # Panics
    ///
    /// Panics if the listener thread handle was already consumed, which violates the runtime
    /// ownership invariant.
    pub fn join(mut self) -> io::Result<()> {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return self
                .thread
                .take()
                .expect("finished HTTP/3 listener thread")
                .join()
                .map_err(|_| io::Error::other("HTTP/3 listener thread panicked"));
        }
        self.thread
            .take()
            .expect("HTTP/3 listener thread exists")
            .join()
            .map_err(|_| io::Error::other("HTTP/3 listener thread panicked"))
    }
}

#[allow(clippy::too_many_arguments)]
fn run_forward(
    listener_name: &str,
    reservation: ListenerReservation,
    service: Arc<ForwardHttp1ServicePlan>,
    tls: Arc<TlsProfilePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    shutdown: watch::Receiver<bool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), Box<dyn Error>> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async move {
        let socket = reservation.duplicate_udp_socket()?;
        let server_config = server_config(&tls, None)?;
        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(server_config),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        metrics.mark_listening();
        ready
            .send(Ok(()))
            .map_err(|_| io::Error::other("HTTP/3 startup receiver was dropped"))?;
        let listener_metrics = metrics.clone();
        let orderly = serve_endpoint(
            listener_name,
            endpoint,
            service,
            generation,
            metrics,
            shutdown,
        )
        .await;
        if orderly {
            listener_metrics.mark_stopped();
        } else {
            listener_metrics.mark_failed();
            return Err(io::Error::other("HTTP/3 endpoint terminated unexpectedly").into());
        }
        Ok::<(), Box<dyn Error>>(())
    })
}

#[allow(clippy::too_many_arguments)]
fn run_reverse(
    listener_name: &str,
    reservation: ListenerReservation,
    service: Arc<HttpServicePlan>,
    tls: Arc<TlsProfilePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    downstream_timeouts: DownstreamTimeoutPolicy,
    shutdown: watch::Receiver<bool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), Box<dyn Error>> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async move {
        let socket = reservation.duplicate_udp_socket()?;
        let server_config = server_config(
            &tls,
            downstream_timeouts
                .keepalive_timeout_ms
                .map(Duration::from_millis),
        )?;
        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(server_config),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        metrics.mark_listening();
        ready
            .send(Ok(()))
            .map_err(|_| io::Error::other("reverse HTTP/3 startup receiver was dropped"))?;
        let listener_metrics = metrics.clone();
        let orderly = serve_reverse_endpoint(
            listener_name,
            endpoint,
            service,
            generation,
            metrics,
            downstream_timeouts,
            shutdown,
        )
        .await;
        if orderly {
            listener_metrics.mark_stopped();
        } else {
            listener_metrics.mark_failed();
            return Err(io::Error::other("reverse HTTP/3 endpoint terminated unexpectedly").into());
        }
        Ok::<(), Box<dyn Error>>(())
    })
}

fn server_config(
    tls: &Arc<TlsProfilePlan>,
    idle_timeout: Option<Duration>,
) -> io::Result<ServerConfig> {
    let resolver = Arc::new(QuicCertificateResolver {
        profile: Arc::clone(tls),
    });
    let mut crypto =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_cert_resolver(resolver);
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    crypto.max_early_data_size = 0;
    let mut config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).map_err(io::Error::other)?,
    ));
    config
        .max_incoming(H3_HANDSHAKE_LIMIT)
        .incoming_buffer_size(H3_INCOMING_BUFFER)
        .incoming_buffer_size_total(H3_TOTAL_INCOMING_BUFFER)
        .migration(false);
    let mut transport = TransportConfig::default();
    let idle_timeout = idle_timeout
        .unwrap_or(H3_CONNECTION_IDLE_TIMEOUT)
        .try_into()
        .map_err(io::Error::other)?;
    transport
        .max_concurrent_bidi_streams(VarInt::from_u32(H3_BIDI_STREAM_LIMIT))
        .max_concurrent_uni_streams(VarInt::from_u32(H3_UNI_STREAM_LIMIT))
        .max_idle_timeout(Some(idle_timeout))
        .stream_receive_window(VarInt::from_u32(H3_STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(H3_CONNECTION_RECEIVE_WINDOW))
        .allow_spin(false)
        .datagram_receive_buffer_size(None)
        .datagram_send_buffer_size(0);
    config.transport_config(Arc::new(transport));
    Ok(config)
}

async fn serve_endpoint(
    listener_name: &str,
    endpoint: Endpoint,
    service: Arc<ForwardHttp1ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    mut shutdown: watch::Receiver<bool>,
) -> bool {
    let handshakes = Arc::new(Semaphore::new(H3_HANDSHAKE_LIMIT));
    let mut connections = tokio::task::JoinSet::new();
    let orderly = loop {
        tokio::select! {
            _ = shutdown.changed() => break true,
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break false };
                let Ok(handshake) = Arc::clone(&handshakes).try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let service = Arc::clone(&service);
                let generation = Arc::clone(&generation);
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                connections.spawn(async move {
                    run_connection(incoming, service, generation, metrics, shutdown, handshake).await;
                });
            }
        }
    };
    endpoint.close(H3_CLOSE_CODE, b"generation draining");
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            warn!("HTTP/3 listener `{listener_name}` connection task failed: {error}");
        }
    }
    orderly
}

async fn serve_reverse_endpoint(
    listener_name: &str,
    endpoint: Endpoint,
    service: Arc<HttpServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    downstream_timeouts: DownstreamTimeoutPolicy,
    mut shutdown: watch::Receiver<bool>,
) -> bool {
    let handshakes = Arc::new(Semaphore::new(H3_HANDSHAKE_LIMIT));
    let mut connections = tokio::task::JoinSet::new();
    let orderly = loop {
        tokio::select! {
            _ = shutdown.changed() => break true,
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break false };
                let Ok(handshake) = Arc::clone(&handshakes).try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let service = Arc::clone(&service);
                let generation = Arc::clone(&generation);
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                connections.spawn(async move {
                    run_reverse_connection(
                        incoming,
                        service,
                        generation,
                        metrics,
                        downstream_timeouts,
                        shutdown,
                        handshake,
                    )
                    .await;
                });
            }
        }
    };
    endpoint.close(H3_CLOSE_CODE, b"generation draining");
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            warn!("reverse HTTP/3 listener `{listener_name}` connection task failed: {error}");
        }
    }
    orderly
}

#[allow(clippy::too_many_arguments)]
async fn run_reverse_connection(
    incoming: quinn::Incoming,
    service: Arc<HttpServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    downstream_timeouts: DownstreamTimeoutPolicy,
    mut shutdown: watch::Receiver<bool>,
    _handshake: OwnedSemaphorePermit,
) {
    let connection = match incoming.await {
        Ok(connection) => connection,
        Err(error) => {
            warn!("reverse HTTP/3 QUIC handshake failed: {error}");
            return;
        }
    };
    let Some(generation_reference) = generation.begin_reference(RuntimeReferenceKind::Http3) else {
        connection.close(H3_CLOSE_CODE, b"generation draining");
        return;
    };
    let listener_connection = match metrics.begin_connection() {
        Ok(connection) => connection,
        Err(error) => {
            warn!("reverse HTTP/3 connection admission failed: {error}");
            connection.close(H3_CLOSE_CODE, b"listener connection limit");
            return;
        }
    };
    let mut builder = h3::server::builder();
    builder
        .max_field_section_size(H3_MAX_FIELD_SECTION_BYTES)
        .send_grease(false);
    let mut h3: h3::server::Connection<_, Bytes> = match builder
        .build(h3_quinn::Connection::new(connection.clone()))
        .await
    {
        Ok(connection) => connection,
        Err(error) => {
            warn!("reverse HTTP/3 connection setup failed: {error}");
            return;
        }
    };
    let request_slots = Arc::new(Semaphore::new(H3_BIDI_STREAM_LIMIT as usize));
    let connector = Arc::new(Connector::new(None));
    let mut requests = tokio::task::JoinSet::new();
    let client_addr = Some(connection.remote_address());
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => break,
                    Err(error) => {
                        warn!("reverse HTTP/3 request acceptance failed: {error}");
                        break;
                    }
                };
                let service = Arc::clone(&service);
                let connector = Arc::clone(&connector);
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                let request_slots = Arc::clone(&request_slots);
                requests.spawn(async move {
                    handle_reverse_request(
                        resolver,
                        service,
                        connector,
                        metrics,
                        client_addr,
                        downstream_timeouts,
                        shutdown,
                        request_slots,
                    )
                    .await;
                });
            }
        }
    }
    connection.close(H3_CLOSE_CODE, b"generation draining");
    while let Some(result) = requests.join_next().await {
        if let Err(error) = result {
            warn!("reverse HTTP/3 request task failed: {error}");
        }
    }
    drop(listener_connection);
    drop(generation_reference);
}

async fn run_connection(
    incoming: quinn::Incoming,
    service: Arc<ForwardHttp1ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    mut shutdown: watch::Receiver<bool>,
    _handshake: tokio::sync::OwnedSemaphorePermit,
) {
    let connection = match incoming.await {
        Ok(connection) => connection,
        Err(error) => {
            warn!("HTTP/3 QUIC handshake failed: {error}");
            return;
        }
    };
    let client_addr = Some(connection.remote_address());
    let Some(service_connection) = service.begin_connection() else {
        connection.close(H3_CLOSE_CODE, b"service connection limit");
        return;
    };
    let Some(generation_reference) = generation.begin_reference(RuntimeReferenceKind::ForwardHttp3)
    else {
        connection.close(H3_CLOSE_CODE, b"generation draining");
        return;
    };
    let listener_connection = match metrics.begin_connection() {
        Ok(connection) => connection,
        Err(error) => {
            warn!("HTTP/3 connection admission failed: {error}");
            connection.close(H3_CLOSE_CODE, b"listener connection limit");
            return;
        }
    };
    let mut builder = h3::server::builder();
    builder
        .max_field_section_size(u64::try_from(service.max_header_bytes()).unwrap_or(u64::MAX))
        .send_grease(false);
    let mut h3: h3::server::Connection<_, bytes::Bytes> = match builder
        .build(h3_quinn::Connection::new(connection.clone()))
        .await
    {
        Ok(connection) => connection,
        Err(error) => {
            warn!("HTTP/3 connection setup failed: {error}");
            return;
        }
    };
    let mut requests = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => break,
                    Err(error) => {
                        warn!("HTTP/3 request acceptance failed: {error}");
                        break;
                    }
                };
                let service = Arc::clone(&service);
                let shutdown = shutdown.clone();
                requests.spawn(async move {
                    handle_request(resolver, service, client_addr, shutdown).await;
                });
            }
        }
    }
    connection.close(H3_CLOSE_CODE, b"generation draining");
    while let Some(result) = requests.join_next().await {
        if let Err(error) = result {
            warn!("HTTP/3 request task failed: {error}");
        }
    }
    drop(listener_connection);
    drop(generation_reference);
    drop(service_connection);
}

#[allow(clippy::too_many_arguments)]
async fn handle_reverse_request<C>(
    resolver: RequestResolver<C, Bytes>,
    service: Arc<HttpServicePlan>,
    connector: Arc<Connector>,
    metrics: ListenerMetrics,
    client_addr: Option<SocketAddr>,
    downstream_timeouts: DownstreamTimeoutPolicy,
    mut shutdown: watch::Receiver<bool>,
    request_slots: Arc<Semaphore>,
) where
    C: h3::quic::Connection<Bytes> + Send + 'static,
    C::BidiStream: h3::quic::BidiStream<Bytes> + Send + 'static,
{
    let Ok((request, mut stream)) = resolver.resolve_request().await else {
        return;
    };
    let Ok(_request_slot) = request_slots.try_acquire_owned() else {
        let deadline = Instant::now() + Duration::from_secs(1);
        let _ = send_h3_response(
            &mut stream,
            StatusCode::SERVICE_UNAVAILABLE,
            &[],
            Bytes::new(),
            false,
            deadline,
        )
        .await;
        return;
    };

    let started_at = Instant::now();
    let deadline = request_deadline(&service, downstream_timeouts, started_at);
    let method = request.method().clone();
    let uri = request.uri().clone();
    let authority = request_authority(&request);
    let response_status = if authority.is_none() {
        send_h3_error(
            &mut stream,
            StatusCode::BAD_REQUEST,
            b"request authority is required\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_REQUEST.as_u16())
    } else if header_bytes(request.headers())
        > usize::try_from(H3_MAX_FIELD_SECTION_BYTES).expect("H3 field limit fits usize")
    {
        send_h3_error(
            &mut stream,
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            b"request headers exceed the HTTP/3 limit\n",
            deadline,
        )
        .await
        .then_some(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.as_u16())
    } else if !is_unambiguous_http_path(uri.path()) {
        send_h3_error(
            &mut stream,
            StatusCode::BAD_REQUEST,
            b"request path is invalid\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_REQUEST.as_u16())
    } else {
        let dispatch_shutdown = shutdown.clone();
        tokio::select! {
            _ = shutdown.changed() => None,
            response = dispatch_reverse_request(
                &mut stream,
                &request,
                authority.as_ref().expect("authority checked above"),
                &service,
                &connector,
                &metrics,
                client_addr,
                deadline,
                dispatch_shutdown,
            ) => response,
        }
    };

    let result = response_status.map_or(HttpOperationResult::Cancelled, |status| {
        HttpOperationResult::from_status(Some(status))
    });
    if let Some(access_log) = service.access_log() {
        if let Err(error) = access_log.write(&serde_json::json!({
            "timestampUnixMs": unix_time_ms(),
            "service": access_log.service(),
            "protocol": "h3",
            "host": authority.as_ref().map(Authority::host),
            "method": method.as_str(),
            "status": response_status,
            "clientIp": client_addr.map(|address| address.ip().to_string()),
            "durationMs": started_at.elapsed().as_millis().to_string(),
        })) {
            warn!("reverse HTTP/3 access log write failed: {error}");
        }
    }
    if let Err(error) = metrics.record_http_operation(result, started_at.elapsed()) {
        warn!("reverse HTTP/3 operation metrics failed: {error}");
    }
}

#[allow(
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::too_many_arguments
)]
#[allow(clippy::too_many_lines)]
async fn dispatch_reverse_request<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    authority: &Authority,
    service: &HttpServicePlan,
    connector: &Connector,
    metrics: &ListenerMetrics,
    client_addr: Option<SocketAddr>,
    deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let method = request.method();
    let uri = request.uri();
    let Some(route) = service.select_route(Some(authority), uri, method) else {
        return send_h3_error(
            stream,
            StatusCode::NOT_FOUND,
            b"route not found\n",
            deadline,
        )
        .await
        .then_some(StatusCode::NOT_FOUND.as_u16());
    };
    if let Some(access) = &route.access {
        if !access.authorizes(request.headers()).await {
            let headers = [(http::header::WWW_AUTHENTICATE, access.challenge().clone())];
            return send_h3_response(
                stream,
                StatusCode::UNAUTHORIZED,
                &headers,
                Bytes::new(),
                false,
                deadline,
            )
            .await
            .then_some(StatusCode::UNAUTHORIZED.as_u16());
        }
    }

    match &route.action {
        HttpActionPlan::Fixed(response) => {
            let headers = response.headers.to_vec();
            return send_h3_response(
                stream,
                StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                &headers,
                response.body.clone(),
                *method == Method::HEAD,
                deadline,
            )
            .await
            .then_some(response.status);
        }
        HttpActionPlan::Redirect(redirect) => {
            let Some(location) = h3_redirect_location(&redirect.location, authority, uri) else {
                return send_h3_error(
                    stream,
                    StatusCode::BAD_REQUEST,
                    b"redirect location is invalid\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_REQUEST.as_u16());
            };
            let mut headers = redirect.headers.to_vec();
            headers.push((http::header::LOCATION, location));
            return send_h3_response(
                stream,
                StatusCode::from_u16(redirect.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                &headers,
                Bytes::new(),
                *method == Method::HEAD,
                deadline,
            )
            .await
            .then_some(redirect.status);
        }
        HttpActionPlan::Static(_) => {
            return send_h3_error(
                stream,
                StatusCode::NOT_IMPLEMENTED,
                b"static HTTP/3 reverse routes are not enabled\n",
                deadline,
            )
            .await
            .then_some(StatusCode::NOT_IMPLEMENTED.as_u16());
        }
        HttpActionPlan::Proxy(proxy) => {
            let body_limit = match (
                service.max_request_body_bytes(),
                route.policy.max_request_body_bytes,
            ) {
                (Some(service_limit), Some(route_limit)) => service_limit.min(route_limit),
                _ => {
                    return send_h3_error(
                        stream,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        b"HTTP/3 route has no bounded request body\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::INTERNAL_SERVER_ERROR.as_u16());
                }
            };
            let content_length = match request_content_length(request.headers()) {
                Ok(length) => length,
                Err(()) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_REQUEST,
                        b"request content-length is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_REQUEST.as_u16());
                }
            };
            if content_length.is_some_and(|length| length > body_limit) {
                return send_h3_error(
                    stream,
                    StatusCode::PAYLOAD_TOO_LARGE,
                    b"request body exceeds the configured limit\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::PAYLOAD_TOO_LARGE.as_u16());
            }
            let body = match recv_reverse_body(
                stream,
                body_limit,
                content_length,
                deadline,
                &mut shutdown,
            )
            .await
            {
                Ok(body) => body,
                Err(ReverseBodyError::TooLarge) => {
                    return send_h3_error(
                        stream,
                        StatusCode::PAYLOAD_TOO_LARGE,
                        b"request body exceeds the configured limit\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::PAYLOAD_TOO_LARGE.as_u16());
                }
                Err(ReverseBodyError::Invalid) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_REQUEST,
                        b"request body is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_REQUEST.as_u16());
                }
                Err(ReverseBodyError::Timeout) => {
                    return send_h3_error(
                        stream,
                        StatusCode::GATEWAY_TIMEOUT,
                        b"request timed out\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::GATEWAY_TIMEOUT.as_u16());
                }
                Err(ReverseBodyError::Cancelled) => return None,
            };
            let _ = metrics.record_bytes_received(u64::try_from(body.len()).unwrap_or(u64::MAX));
            let mut selected = match timeout_at(deadline, proxy.pool.select_endpoint(&[])).await {
                Ok(Ok(selected)) => selected,
                Ok(Err(_)) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_GATEWAY,
                        b"upstream endpoint is unavailable\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_GATEWAY.as_u16());
                }
                Err(_) => {
                    return send_h3_error(
                        stream,
                        StatusCode::GATEWAY_TIMEOUT,
                        b"upstream selection timed out\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::GATEWAY_TIMEOUT.as_u16());
                }
            };
            let upstream_host = match selected_upstream_host(
                selected.endpoint(),
                proxy.policy.upstream_host.clone(),
                Some(authority),
            ) {
                Ok(Some(host)) => host,
                Ok(None) => HeaderValue::from_static(""),
                Err(_) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_GATEWAY,
                        b"upstream host policy is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_GATEWAY.as_u16());
                }
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            let connect_timeout = remaining.min(route.policy.connect_timeout);
            let read_timeout = remaining.min(route.policy.read_timeout);
            let write_timeout = remaining.min(route.policy.write_timeout);
            let peer = match timeout_at(
                deadline,
                selected.prepare_peer_with_timeouts(
                    proxy.pool.as_ref(),
                    connect_timeout,
                    read_timeout,
                    write_timeout,
                ),
            )
            .await
            {
                Ok(Ok(peer)) => peer,
                Ok(Err(_)) | Err(_) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_GATEWAY,
                        b"upstream connection failed\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_GATEWAY.as_u16());
                }
            };
            let mut upstream = match timeout_at(deadline, connector.get_http_session(&peer)).await {
                Ok(Ok((upstream, _reused))) => upstream,
                Ok(Err(_)) | Err(_) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_GATEWAY,
                        b"upstream HTTP session failed\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_GATEWAY.as_u16());
                }
            };
            upstream.set_read_timeout(Some(read_timeout));
            upstream.set_write_timeout(Some(write_timeout));
            let request_header = match build_upstream_request(
                request,
                &body,
                &upstream_host,
                &proxy.policy,
                client_addr,
            ) {
                Ok(request) => request,
                Err(()) => {
                    upstream.shutdown().await;
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_GATEWAY,
                        b"upstream request policy is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_GATEWAY.as_u16());
                }
            };
            if timeout_at(deadline, upstream.write_request_header(request_header))
                .await
                .is_err()
                || timeout_at(deadline, upstream.write_request_body(body, true))
                    .await
                    .is_err()
                || timeout_at(deadline, upstream.finish_request_body())
                    .await
                    .is_err()
            {
                upstream.shutdown().await;
                return send_h3_error(
                    stream,
                    StatusCode::BAD_GATEWAY,
                    b"upstream request write failed\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_GATEWAY.as_u16());
            }
            if validate_tls_connection(proxy.pool.tls(), upstream.digest()).is_err() {
                upstream.shutdown().await;
                return send_h3_error(
                    stream,
                    StatusCode::BAD_GATEWAY,
                    b"upstream TLS policy rejected the connection\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_GATEWAY.as_u16());
            }
            if timeout_at(deadline, upstream.read_response_header())
                .await
                .is_err()
            {
                upstream.shutdown().await;
                return send_h3_error(
                    stream,
                    StatusCode::BAD_GATEWAY,
                    b"upstream response header failed\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_GATEWAY.as_u16());
            }
            let Some(mut response_header) = upstream.response_header().cloned() else {
                upstream.shutdown().await;
                return send_h3_error(
                    stream,
                    StatusCode::BAD_GATEWAY,
                    b"upstream response header is unavailable\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_GATEWAY.as_u16());
            };
            if remove_upstream_hop_by_hop_response_headers(&mut response_header).is_err()
                || apply_response_policy(&mut response_header, &proxy.policy).is_err()
            {
                upstream.shutdown().await;
                return send_h3_error(
                    stream,
                    StatusCode::BAD_GATEWAY,
                    b"upstream response headers are invalid\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_GATEWAY.as_u16());
            }
            let status = response_header.status.as_u16();
            let response = match h3_response_from_pingora(&response_header) {
                Ok(response) => response,
                Err(()) => {
                    upstream.shutdown().await;
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_GATEWAY,
                        b"upstream response cannot be represented as HTTP/3\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_GATEWAY.as_u16());
                }
            };
            if !matches!(
                timeout_at(deadline, stream.send_response(response)).await,
                Ok(Ok(()))
            ) {
                upstream.shutdown().await;
                return None;
            }
            let mut sent = 0_u64;
            loop {
                let chunk = match timeout_at(deadline, upstream.read_response_body()).await {
                    Ok(Ok(Some(chunk))) => chunk,
                    Ok(Ok(None)) => break,
                    Ok(Err(_)) | Err(_) => {
                        upstream.shutdown().await;
                        return Some(status);
                    }
                };
                let next = sent.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                if next > H3_MAX_RESPONSE_BODY_BYTES {
                    stream.stop_stream(h3::error::Code::H3_EXCESSIVE_LOAD);
                    stream.stop_sending(h3::error::Code::H3_EXCESSIVE_LOAD);
                    upstream.shutdown().await;
                    return Some(status);
                }
                sent = next;
                let _ = metrics.record_bytes_sent(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                if !matches!(
                    timeout_at(deadline, stream.send_data(chunk)).await,
                    Ok(Ok(()))
                ) {
                    upstream.shutdown().await;
                    return None;
                }
            }
            let _ = timeout_at(deadline, stream.finish()).await;
            connector.release_http_session(upstream, &peer, None).await;
            Some(status)
        }
    }
}

fn request_deadline(
    service: &HttpServicePlan,
    downstream_timeouts: DownstreamTimeoutPolicy,
    started_at: Instant,
) -> Instant {
    let mut timeout = service.upstream_io_timeout();
    for value in [
        downstream_timeouts.client_timeout_ms,
        downstream_timeouts.request_timeout_ms,
    ]
    .into_iter()
    .flatten()
    {
        timeout = timeout.min(Duration::from_millis(value));
    }
    started_at + timeout
}

fn request_authority(request: &Request<()>) -> Option<Authority> {
    request.uri().authority().cloned().or_else(|| {
        request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    })
}

fn header_bytes(headers: &HeaderMap) -> usize {
    headers.iter().fold(0, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
    })
}

fn request_content_length(headers: &HeaderMap) -> Result<Option<u64>, ()> {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let length = values
        .next()
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .ok_or(())
        })
        .transpose()?;
    if values.next().is_some() {
        return Err(());
    }
    Ok(length)
}

#[derive(Clone, Copy)]
enum ReverseBodyError {
    TooLarge,
    Invalid,
    Timeout,
    Cancelled,
}

async fn recv_reverse_body<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    limit: u64,
    expected_length: Option<u64>,
    deadline: Instant,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Bytes, ReverseBodyError>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            changed = shutdown.changed() => {
                let _ = changed;
                return Err(ReverseBodyError::Cancelled);
            }
            result = timeout_at(deadline, stream.recv_data()) => match result {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(_)) => return Err(ReverseBodyError::Invalid),
                Err(_) => return Err(ReverseBodyError::Timeout),
            },
        };
        let Some(chunk) = next else {
            break;
        };
        let mut chunk = chunk;
        let chunk_length = chunk.remaining();
        let chunk = chunk.copy_to_bytes(chunk_length);
        let next_length = body
            .len()
            .checked_add(chunk.len())
            .and_then(|length| u64::try_from(length).ok())
            .ok_or(ReverseBodyError::TooLarge)?;
        if next_length > limit {
            stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
            return Err(ReverseBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    if expected_length.is_some_and(|length| length != u64::try_from(body.len()).unwrap_or(u64::MAX))
    {
        return Err(ReverseBodyError::Invalid);
    }
    Ok(Bytes::from(body))
}

async fn send_h3_error<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    status: StatusCode,
    body: &[u8],
    deadline: Instant,
) -> bool
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    send_h3_response(
        stream,
        status,
        &[],
        Bytes::copy_from_slice(body),
        false,
        deadline,
    )
    .await
}

async fn send_h3_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    status: StatusCode,
    headers: &[(HeaderName, HeaderValue)],
    body: Bytes,
    head: bool,
    deadline: Instant,
) -> bool
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let body_forbidden = matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let Ok(mut response) = Response::builder().status(status).body(()) else {
        return false;
    };
    for (name, value) in headers {
        response.headers_mut().append(name.clone(), value.clone());
    }
    if body_forbidden {
        response.headers_mut().remove(CONTENT_LENGTH);
    } else if !response.headers().contains_key(CONTENT_LENGTH) {
        let length = if head { 0 } else { body.len() };
        response.headers_mut().insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).expect("body length is a valid header"),
        );
    }
    if !matches!(
        timeout_at(deadline, stream.send_response(response)).await,
        Ok(Ok(()))
    ) {
        return false;
    }
    if !head
        && !body_forbidden
        && !body.is_empty()
        && !matches!(
            timeout_at(deadline, stream.send_data(body)).await,
            Ok(Ok(()))
        )
    {
        return false;
    }
    matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(())))
}

fn h3_redirect_location(
    location: &HttpRedirectLocation,
    authority: &Authority,
    uri: &Uri,
) -> Option<HeaderValue> {
    let value = match location {
        HttpRedirectLocation::Literal { value } => value.clone(),
        HttpRedirectLocation::RequestTemplate {
            value,
            nginx_host_fallback,
        } => {
            let request_uri = uri
                .path_and_query()
                .map_or(uri.path(), |value| value.as_str());
            let host = normalized_h3_host(authority)
                .or_else(|| nginx_host_fallback.as_deref().map(str::to_owned))
                .unwrap_or_default();
            let mut expanded = String::with_capacity(value.len() + request_uri.len());
            let mut remainder = value.as_str();
            while let Some((literal, variable)) = remainder.split_once('$') {
                expanded.push_str(literal);
                if let Some(after) = variable.strip_prefix("scheme") {
                    expanded.push_str("https");
                    remainder = after;
                } else if let Some(after) = variable.strip_prefix("host") {
                    expanded.push_str(&host);
                    remainder = after;
                } else if let Some(after) = variable.strip_prefix("request_uri") {
                    expanded.push_str(request_uri);
                    remainder = after;
                } else {
                    return None;
                }
            }
            expanded.push_str(remainder);
            expanded
        }
    };
    (value.len() <= 8 * 1024)
        .then(|| HeaderValue::from_str(&value).ok())
        .flatten()
}

fn build_upstream_request(
    request: &Request<()>,
    body: &Bytes,
    upstream_host: &HeaderValue,
    policy: &ProxyPolicyPlan,
    client_addr: Option<SocketAddr>,
) -> Result<Box<RequestHeader>, ()> {
    let mut uri = request.uri().clone();
    if let Some(rewrite) = &policy.upstream_path_rewrite {
        rewrite_upstream_path(&mut uri, rewrite).map_err(|_| ())?;
    }
    let path = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    let mut upstream =
        RequestHeader::build(request.method().clone(), path.as_bytes(), Some(1)).map_err(|_| ())?;
    for (name, value) in request.headers() {
        if name == HOST || name == CONTENT_LENGTH || h3_hop_by_hop(name) {
            continue;
        }
        upstream
            .append_header(name.clone(), value.clone())
            .map_err(|_| ())?;
    }
    upstream
        .insert_header(HOST, upstream_host.clone())
        .map_err(|_| ())?;
    upstream
        .insert_header(CONTENT_LENGTH, body.len().to_string())
        .map_err(|_| ())?;
    let authority = request_authority(request).ok_or(())?;
    apply_h3_request_mutations(
        request,
        &mut upstream,
        &authority,
        policy,
        client_addr,
        upstream_host,
    )?;
    Ok(Box::new(upstream))
}

fn apply_h3_request_mutations(
    incoming: &Request<()>,
    upstream: &mut RequestHeader,
    authority: &Authority,
    policy: &ProxyPolicyPlan,
    client_addr: Option<SocketAddr>,
    selected_upstream_host: &HeaderValue,
) -> Result<(), ()> {
    for mutation in &policy.request_headers {
        if mutation.is_pingora_managed_upgrade() {
            continue;
        }
        match mutation {
            RequestHeaderMutationPlan::Remove { name } => {
                upstream.remove_header(name);
            }
            RequestHeaderMutationPlan::Set { name, value } => {
                let value = match value {
                    RequestHeaderValuePlan::Literal(value) => value.clone(),
                    RequestHeaderValuePlan::IncomingAuthority => {
                        HeaderValue::from_str(authority.as_str()).map_err(|_| ())?
                    }
                    RequestHeaderValuePlan::NormalizedHost => {
                        HeaderValue::from_str(&normalized_h3_host(authority).unwrap_or_default())
                            .map_err(|_| ())?
                    }
                    RequestHeaderValuePlan::NginxHost { fallback } => {
                        nginx_h3_host(authority).unwrap_or_else(|| fallback.clone())
                    }
                    RequestHeaderValuePlan::ClientIp => {
                        HeaderValue::from_str(&client_addr.ok_or(())?.ip().to_string())
                            .map_err(|_| ())?
                    }
                    RequestHeaderValuePlan::AppendedXForwardedFor {
                        max_bytes,
                        except_source_cidrs,
                    } => {
                        if client_addr.is_some_and(|address| {
                            except_source_cidrs
                                .iter()
                                .any(|cidr| cidr.contains(address.ip()))
                        }) {
                            continue;
                        }
                        let mut value = joined_h3_headers(
                            incoming.headers(),
                            &HeaderName::from_static("x-forwarded-for"),
                            *max_bytes,
                        )?
                        .unwrap_or_default();
                        if let Some(address) = client_addr {
                            if !value.is_empty() {
                                value.extend_from_slice(b", ");
                            }
                            value.extend_from_slice(address.ip().to_string().as_bytes());
                        }
                        if value.len() > *max_bytes {
                            return Err(());
                        }
                        HeaderValue::from_bytes(&value).map_err(|_| ())?
                    }
                    RequestHeaderValuePlan::DownstreamScheme => HeaderValue::from_static("https"),
                    RequestHeaderValuePlan::IncomingHeader { name, max_bytes } => {
                        HeaderValue::from_bytes(
                            &joined_h3_headers(incoming.headers(), name, *max_bytes)?
                                .unwrap_or_default(),
                        )
                        .map_err(|_| ())?
                    }
                    RequestHeaderValuePlan::SelectedUpstreamHost => selected_upstream_host.clone(),
                };
                upstream
                    .insert_header(name.clone(), value)
                    .map_err(|_| ())?;
            }
        }
    }
    Ok(())
}

fn joined_h3_headers(
    headers: &HeaderMap,
    name: &HeaderName,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, ()> {
    let mut joined = Vec::new();
    for value in headers.get_all(name) {
        if !joined.is_empty() {
            joined.extend_from_slice(b", ");
        }
        if joined.len().saturating_add(value.as_bytes().len()) > max_bytes {
            return Err(());
        }
        joined.extend_from_slice(value.as_bytes());
    }
    Ok((!joined.is_empty()).then_some(joined))
}

fn h3_response_from_pingora(response: &pingora::http::ResponseHeader) -> Result<Response<()>, ()> {
    let mut output = Response::builder()
        .status(response.status)
        .body(())
        .map_err(|_| ())?;
    for (name, value) in &response.headers {
        output.headers_mut().append(name.clone(), value.clone());
    }
    Ok(output)
}

fn h3_hop_by_hop(name: &HeaderName) -> bool {
    name == CONNECTION
        || name == KEEP_ALIVE
        || name == PROXY_CONNECTION
        || name == TE
        || name == TRAILER
        || name == TRANSFER_ENCODING
        || name == UPGRADE
}

fn normalized_h3_host(authority: &Authority) -> Option<String> {
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed
        .parse::<IpAddr>()
        .map(|ip| ip.to_string())
        .ok()
        .or_else(|| Some(host.to_ascii_lowercase()))
}

fn nginx_h3_host(authority: &Authority) -> Option<HeaderValue> {
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let value = unbracketed.parse::<IpAddr>().map_or_else(
        |_| host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase(),
        |ip| match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        },
    );
    HeaderValue::from_str(&value).ok()
}

fn unix_time_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

pub(crate) fn capability_snapshot(listeners: &[crate::ListenerSnapshot]) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "http3": {
            "reverse": listener_capability(listeners, "http3"),
            "forward": listener_capability(listeners, "forward_http3"),
        },
    })
}

fn listener_capability(listeners: &[crate::ListenerSnapshot], protocol: &str) -> serde_json::Value {
    let configured = listeners
        .iter()
        .filter(|listener| listener.protocol == protocol)
        .collect::<Vec<_>>();
    let active = configured
        .iter()
        .filter(|listener| {
            listener.state == ListenerRuntimeState::Listening
                && listener.administrative_state == crate::AdministrativeState::Ready
        })
        .map(|listener| listener.name.clone())
        .collect::<Vec<_>>();
    let status = if !active.is_empty() {
        "active"
    } else if configured.is_empty() {
        "unconfigured"
    } else {
        "blocked"
    };
    serde_json::json!({
        "status": status,
        "listeners": active,
        "configuredListeners": configured.iter().map(|listener| listener.name.clone()).collect::<Vec<_>>(),
        "transport": "quic",
        "alpn": ["h3"],
        "tlsMinVersion": "1.3",
        "zeroRtt": "disabled",
        "migration": "disabled",
        "fallback": "none",
        "limits": {
            "maxHandshakesAndConnections": H3_HANDSHAKE_LIMIT,
            "maxBidirectionalStreams": H3_BIDI_STREAM_LIMIT,
            "maxUnidirectionalStreams": H3_UNI_STREAM_LIMIT,
            "maxFieldSectionBytes": H3_MAX_FIELD_SECTION_BYTES,
            "maxResponseBodyBytes": H3_MAX_RESPONSE_BODY_BYTES,
        },
        "blockedReason": (status == "blocked").then_some("listener is not listening"),
    })
}

async fn handle_request<C>(
    resolver: RequestResolver<C, bytes::Bytes>,
    service: Arc<ForwardHttp1ServicePlan>,
    client_addr: Option<std::net::SocketAddr>,
    shutdown: watch::Receiver<bool>,
) where
    C: h3::quic::Connection<bytes::Bytes> + Send + 'static,
    C::BidiStream: h3::quic::BidiStream<bytes::Bytes> + Send + 'static,
{
    let Ok((request, stream)) = resolver.resolve_request().await else {
        return;
    };
    service
        .handle_h3(request, stream, client_addr, shutdown)
        .await;
}

#[derive(Debug)]
struct QuicCertificateResolver {
    profile: Arc<TlsProfilePlan>,
}

impl ResolvesServerCert for QuicCertificateResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        self.profile
            .selected_generation(client_hello.server_name())
            .quic_certified_key()
            .map(Arc::new)
            .map_err(|error| {
                warn!("QUIC certificate selection failed: {error}");
            })
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdministrativeState, ListenerSnapshot};

    #[test]
    fn reports_reverse_capability_only_for_listening_ready_listeners() {
        let listeners = vec![ListenerSnapshot {
            administrative_state: AdministrativeState::Ready,
            name: "reverse".into(),
            protocol: "http3".into(),
            bind: "udp://127.0.0.1:9443".into(),
            max_connections: Some(8),
            state: ListenerRuntimeState::Listening,
            accepted_connections: 0,
            rejected_connections: 0,
            active_connections: 0,
            bytes_received: 0,
            bytes_sent: 0,
            http_operations: None,
            tcp_relays: None,
            proxy_protocol: None,
            cache: None,
        }];
        let value = capability_snapshot(&listeners);

        assert_eq!(value["http3"]["reverse"]["status"], "active");
        assert_eq!(value["http3"]["forward"]["status"], "unconfigured");
        assert_eq!(value["http3"]["reverse"]["fallback"], "none");
    }

    #[test]
    fn rejects_duplicate_or_invalid_content_lengths() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("3"));
        assert_eq!(request_content_length(&headers), Ok(Some(3)));

        headers.append(CONTENT_LENGTH, HeaderValue::from_static("4"));
        assert_eq!(request_content_length(&headers), Err(()));

        let mut invalid = HeaderMap::new();
        invalid.insert(CONTENT_LENGTH, HeaderValue::from_static("not-a-number"));
        assert_eq!(request_content_length(&invalid), Err(()));
    }
}
