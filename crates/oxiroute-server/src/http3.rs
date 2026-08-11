use std::{
    error::Error,
    io,
    net::SocketAddr,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use bytes::{Buf as _, Bytes};
use h3::{
    quic::{RecvStream as _, SendStream as _},
    server::RequestResolver,
};
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
    header::{CONNECTION, CONTENT_LENGTH, HOST, TE, TRAILER, TRANSFER_ENCODING, UPGRADE},
    uri::Authority,
};
use log::{error, warn};
use oxiroute_cache::{CachedResponse, ResponseTiming};
use oxiroute_config::{
    DownstreamTimeoutPolicy, HttpRetryTarget, HttpRetryTrigger, is_unambiguous_http_path,
};
use pingora::apps::AcceptGateParticipant;
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Endpoint, EndpointConfig, ServerConfig, TransportConfig, VarInt};
use rustls::server::{ClientHello, ResolvesServerCert};
use tokio::{
    fs::File as TokioFile,
    io::{AsyncReadExt as _, AsyncSeekExt as _},
    runtime::Builder,
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    time::{Instant, timeout_at},
};

use crate::{
    ForwardHttp1ServicePlan, H3UpstreamError, HealthFailure, HttpOperationResult, HttpServicePlan,
    ListenerMetrics, ListenerReservation, ListenerRuntimeState, RuntimeGeneration, RuntimeMode,
    RuntimeReferenceKind, TlsProfilePlan,
    http_action::{
        HttpActionPlan, ProxyActionPlan, ProxyPolicyPlan, RedirectLocationPlan, StaticErrorTarget,
        StaticFile, StaticRequestDecision, StaticServeError, StaticTarget,
    },
    http_cache::{
        CacheFailureClass, CacheRequest, CacheStart, CacheStartFailure, CacheTransaction,
        HttpCachePlan,
    },
    http_policy::{
        RedirectContext, RequestHeaderDecision, RequestPolicyContext, RequestPolicyError,
        decide_request_header, expand_redirect_location, normalized_redirect_host,
    },
    http_proxy::{
        apply_response_policy_map, cache_request_bypasses_cache, rewrite_upstream_path,
        selected_upstream_host,
    },
};

pub(crate) const H3_HANDSHAKE_LIMIT: usize = 64;
pub(crate) const H3_BIDI_STREAM_LIMIT: u32 = 128;
pub(crate) const H3_UNI_STREAM_LIMIT: u32 = 16;
const H3_STREAM_RECEIVE_WINDOW: u32 = 1024 * 1024;
const H3_CONNECTION_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;
const H3_INCOMING_BUFFER: u64 = 1024 * 1024;
const H3_TOTAL_INCOMING_BUFFER: u64 = 16 * 1024 * 1024;
pub(crate) const H3_MAX_FIELD_SECTION_BYTES: u64 = 16 * 1024;
pub(crate) const H3_MAX_REQUEST_BODY_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const H3_MAX_RESPONSE_BODY_BYTES: u64 = 64 * 1024 * 1024;
const H3_CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const H3_GRACEFUL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const H3_CLOSE_CODE: VarInt = VarInt::from_u32(0x100);
const KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
const PROXY_CONNECTION: HeaderName = HeaderName::from_static("proxy-connection");
const MAX_STATIC_REDIRECTS: usize = 10;

enum H3CacheDecision {
    Bypass,
    Respond(Box<CachedResponse>),
    OnlyIfCached,
    Continue(Box<CacheTransaction>),
}

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
    /// service app. Proxy routes require an exact H3 upstream pool and use the native H3 upstream
    /// transport without an H1/H2 fallback.
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
        let accept_gate = generation.accept_gate().register();
        let orderly = serve_endpoint(
            listener_name,
            endpoint,
            service,
            generation,
            metrics,
            shutdown,
            accept_gate,
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
        let accept_gate = generation.accept_gate().register();
        let orderly = serve_reverse_endpoint(
            listener_name,
            endpoint,
            service,
            generation,
            metrics,
            downstream_timeouts,
            shutdown,
            accept_gate,
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
    let crypto_builder =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13]);
    let mut crypto = match tls.h3_client_cert_verifier() {
        Some(verifier) => crypto_builder
            .with_client_cert_verifier(verifier)
            .with_cert_resolver(resolver),
        None => crypto_builder
            .with_no_client_auth()
            .with_cert_resolver(resolver),
    };
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
    mut accept_gate: AcceptGateParticipant,
) -> bool {
    let handshakes = Arc::new(Semaphore::new(H3_HANDSHAKE_LIMIT));
    let mut connections = tokio::task::JoinSet::new();
    let (drain_tx, drain_rx) = watch::channel(false);
    let mut gate_state = accept_gate.state();
    let orderly = loop {
        tokio::select! {
            state = accept_gate.changed() => {
                let Ok(state) = state else { break false };
                let gate_closed = !state.accepting && state.epoch > gate_state.epoch;
                gate_state = state;
                accept_gate.acknowledge(state.epoch);
                if gate_closed {
                    let _ = drain_tx.send(true);
                    break true;
                }
            }
            _ = shutdown.changed() => {
                accept_gate.acknowledge(accept_gate.state().epoch);
                let _ = drain_tx.send(true);
                break true;
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break false };
                if !accept_gate.state().accepting || *drain_rx.borrow() || *shutdown.borrow() {
                    incoming.refuse();
                    continue;
                }
                let Ok(handshake) = Arc::clone(&handshakes).try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let service = Arc::clone(&service);
                let generation = Arc::clone(&generation);
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                let drain = drain_rx.clone();
                connections.spawn(async move {
                    run_connection(
                        incoming,
                        service,
                        generation,
                        metrics,
                        shutdown,
                        drain,
                        handshake,
                    )
                    .await;
                });
            }
        }
    };
    finish_h3_connections(listener_name, &endpoint, &mut connections).await;
    orderly
}

#[allow(clippy::too_many_arguments)]
async fn serve_reverse_endpoint(
    listener_name: &str,
    endpoint: Endpoint,
    service: Arc<HttpServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    downstream_timeouts: DownstreamTimeoutPolicy,
    mut shutdown: watch::Receiver<bool>,
    mut accept_gate: AcceptGateParticipant,
) -> bool {
    let handshakes = Arc::new(Semaphore::new(H3_HANDSHAKE_LIMIT));
    let mut connections = tokio::task::JoinSet::new();
    let (drain_tx, drain_rx) = watch::channel(false);
    let mut gate_state = accept_gate.state();
    let orderly = loop {
        tokio::select! {
            state = accept_gate.changed() => {
                let Ok(state) = state else { break false };
                let gate_closed = !state.accepting && state.epoch > gate_state.epoch;
                gate_state = state;
                accept_gate.acknowledge(state.epoch);
                if gate_closed {
                    let _ = drain_tx.send(true);
                    break true;
                }
            }
            _ = shutdown.changed() => {
                accept_gate.acknowledge(accept_gate.state().epoch);
                let _ = drain_tx.send(true);
                break true;
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break false };
                if !accept_gate.state().accepting || *drain_rx.borrow() || *shutdown.borrow() {
                    incoming.refuse();
                    continue;
                }
                let Ok(handshake) = Arc::clone(&handshakes).try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let service = Arc::clone(&service);
                let generation = Arc::clone(&generation);
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                let drain = drain_rx.clone();
                connections.spawn(async move {
                    run_reverse_connection(
                        incoming,
                        service,
                        generation,
                        metrics,
                        downstream_timeouts,
                        shutdown,
                        drain,
                        handshake,
                    )
                    .await;
                });
            }
        }
    };
    finish_h3_connections(listener_name, &endpoint, &mut connections).await;
    orderly
}

async fn finish_h3_connections(
    listener_name: &str,
    endpoint: &Endpoint,
    connections: &mut tokio::task::JoinSet<()>,
) {
    let deadline = Instant::now() + H3_GRACEFUL_DRAIN_TIMEOUT;
    let mut timed_out = false;
    loop {
        match timeout_at(deadline, connections.join_next()).await {
            Ok(Some(result)) => {
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    warn!("HTTP/3 listener `{listener_name}` connection task failed: {error}");
                }
            }
            Ok(None) => break,
            Err(_) => {
                timed_out = true;
                warn!("HTTP/3 listener `{listener_name}` drain deadline expired");
                break;
            }
        }
    }
    endpoint.close(H3_CLOSE_CODE, b"generation draining");
    if timed_out {
        connections.abort_all();
    }
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            warn!("HTTP/3 listener `{listener_name}` connection task failed: {error}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_reverse_connection(
    incoming: quinn::Incoming,
    service: Arc<HttpServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    downstream_timeouts: DownstreamTimeoutPolicy,
    mut shutdown: watch::Receiver<bool>,
    mut drain: watch::Receiver<bool>,
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
    let mut requests = tokio::task::JoinSet::new();
    let (request_cancel_tx, request_cancel_rx) = watch::channel(false);
    let client_addr = Some(connection.remote_address());
    let graceful = loop {
        tokio::select! {
            _ = drain.changed() => break true,
            _ = shutdown.changed() => break true,
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => break false,
                    Err(error) => {
                        warn!("reverse HTTP/3 request acceptance failed: {error}");
                        break false;
                    }
                };
                if *drain.borrow() || *shutdown.borrow() {
                    reject_h3_resolver(resolver);
                    break true;
                }
                let service = Arc::clone(&service);
                let metrics = metrics.clone();
                let request_cancel = request_cancel_rx.clone();
                let request_slots = Arc::clone(&request_slots);
                requests.spawn(async move {
                    Box::pin(handle_reverse_request(
                        resolver,
                        service,
                        metrics,
                        client_addr,
                        downstream_timeouts,
                        request_cancel,
                        request_slots,
                    ))
                    .await;
                });
            }
        }
    };
    let deadline = Instant::now() + H3_GRACEFUL_DRAIN_TIMEOUT;
    let goaway_sent = if graceful {
        drain_h3_connection(&mut h3, deadline).await
    } else {
        false
    };
    if !goaway_sent {
        let _ = request_cancel_tx.send(true);
        requests.abort_all();
    }
    join_h3_requests(
        "reverse HTTP/3",
        &mut requests,
        &request_cancel_tx,
        deadline,
    )
    .await;
    connection.close(H3_CLOSE_CODE, b"generation draining");
    drop(listener_connection);
    drop(generation_reference);
}

async fn run_connection(
    incoming: quinn::Incoming,
    service: Arc<ForwardHttp1ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    mut shutdown: watch::Receiver<bool>,
    mut drain: watch::Receiver<bool>,
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
    let (request_cancel_tx, request_cancel_rx) = watch::channel(false);
    let graceful = loop {
        tokio::select! {
            _ = drain.changed() => break true,
            _ = shutdown.changed() => break true,
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => break false,
                    Err(error) => {
                        warn!("HTTP/3 request acceptance failed: {error}");
                        break false;
                    }
                };
                if *drain.borrow() || *shutdown.borrow() {
                    reject_h3_resolver(resolver);
                    break true;
                }
                let service = Arc::clone(&service);
                let metrics = metrics.clone();
                let request_cancel = request_cancel_rx.clone();
                requests.spawn(async move {
                    handle_request(resolver, service, client_addr, request_cancel, metrics).await;
                });
            }
        }
    };
    let deadline = Instant::now() + H3_GRACEFUL_DRAIN_TIMEOUT;
    let goaway_sent = if graceful {
        drain_h3_connection(&mut h3, deadline).await
    } else {
        false
    };
    if !goaway_sent {
        let _ = request_cancel_tx.send(true);
        requests.abort_all();
    }
    join_h3_requests("HTTP/3", &mut requests, &request_cancel_tx, deadline).await;
    connection.close(H3_CLOSE_CODE, b"generation draining");
    drop(listener_connection);
    drop(generation_reference);
    drop(service_connection);
}

async fn drain_h3_connection<C>(
    connection: &mut h3::server::Connection<C, Bytes>,
    deadline: Instant,
) -> bool
where
    C: h3::quic::Connection<Bytes>,
{
    if !matches!(
        timeout_at(deadline, connection.shutdown(0)).await,
        Ok(Ok(()))
    ) {
        return false;
    }
    true
}

fn reject_h3_resolver<C>(mut resolver: RequestResolver<C, Bytes>)
where
    C: h3::quic::Connection<Bytes>,
{
    let code = h3::error::Code::H3_REQUEST_REJECTED.value();
    resolver.frame_stream.reset(code);
    resolver.frame_stream.stream.stop_sending(code);
}

async fn join_h3_requests(
    protocol: &str,
    requests: &mut tokio::task::JoinSet<()>,
    request_cancel: &watch::Sender<bool>,
    deadline: Instant,
) {
    let mut timed_out = false;
    loop {
        match timeout_at(deadline, requests.join_next()).await {
            Ok(Some(result)) => {
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    warn!("{protocol} request task failed: {error}");
                }
            }
            Ok(None) => break,
            Err(_) => {
                timed_out = true;
                warn!("{protocol} request drain deadline expired");
                break;
            }
        }
    }
    if timed_out {
        let _ = request_cancel.send(true);
        requests.abort_all();
        while let Some(result) = requests.join_next().await {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                warn!("{protocol} request task failed: {error}");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_reverse_request<C>(
    resolver: RequestResolver<C, Bytes>,
    service: Arc<HttpServicePlan>,
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
    let response_status = if let Some(authority) = authority.as_ref() {
        if header_bytes(request.headers())
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
                    authority,
                    &service,
                    &metrics,
                    client_addr,
                    deadline,
                    dispatch_shutdown,
                ) => response,
            }
        }
    } else {
        send_h3_error(
            &mut stream,
            StatusCode::BAD_REQUEST,
            b"request authority is required\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_REQUEST.as_u16())
    };

    let result = response_status.map_or(HttpOperationResult::Cancelled, |status| {
        HttpOperationResult::from_status(Some(status))
    });
    if let Some(access_log) = service.access_log()
        && let Err(error) = access_log.write(&serde_json::json!({
            "timestampUnixMs": unix_time_ms(),
            "service": access_log.service(),
            "protocol": "h3",
            "host": authority.as_ref().map(Authority::host),
            "method": method.as_str(),
            "status": response_status,
            "clientIp": client_addr.map(|address| address.ip().to_string()),
            "durationMs": started_at.elapsed().as_millis().to_string(),
        }))
    {
        warn!("reverse HTTP/3 access log write failed: {error}");
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
    if let Some(access) = &route.access
        && !access.authorizes(request.headers()).await
    {
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
        HttpActionPlan::Static(files) => {
            return send_h3_static_request(stream, files, request, deadline).await;
        }
        HttpActionPlan::Proxy(proxy) => {
            if method.as_str().eq_ignore_ascii_case("PURGE")
                && proxy
                    .policy
                    .cache
                    .as_ref()
                    .is_some_and(|cache| cache.purge_access.is_some())
            {
                return purge_h3_cache(
                    stream,
                    request,
                    authority,
                    Arc::clone(proxy.policy.cache.as_ref().expect("checked cache plan")),
                    deadline,
                )
                .await;
            }
            let request_header_decisions =
                match h3_request_header_decisions(request, authority, &proxy.policy, client_addr) {
                    Ok(decisions) => decisions,
                    Err(_) => {
                        return send_h3_error(
                            stream,
                            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                            b"request header policy expansion failed\n",
                            deadline,
                        )
                        .await
                        .then_some(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.as_u16());
                    }
                };
            if proxy.pool.h3().is_none() {
                return send_h3_error(
                    stream,
                    StatusCode::BAD_GATEWAY,
                    b"HTTP/3 reverse routes require an exact HTTP/3 upstream pool\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_GATEWAY.as_u16());
            }
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
                stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
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
            let mut cache_transaction = None;
            if body.is_empty()
                && let Some(cache) = proxy.policy.cache.as_ref()
                && cache.allows_method(method)
                && !cache_request_bypasses_cache(request.headers())
            {
                match start_h3_cache(cache, request, authority, metrics).await {
                    H3CacheDecision::Bypass => {}
                    H3CacheDecision::Respond(response) => {
                        let status = conditional_h3_cache_status(request, &response);
                        return send_h3_cached_response(stream, request, *response, deadline)
                            .await
                            .then_some(status.as_u16());
                    }
                    H3CacheDecision::OnlyIfCached => {
                        return send_h3_error(
                            stream,
                            StatusCode::GATEWAY_TIMEOUT,
                            b"cache entry is unavailable\n",
                            deadline,
                        )
                        .await
                        .then_some(StatusCode::GATEWAY_TIMEOUT.as_u16());
                    }
                    H3CacheDecision::Continue(transaction) => {
                        cache_transaction = Some(*transaction);
                    }
                }
            }
            dispatch_h3_upstream_request(
                stream,
                request,
                authority,
                proxy,
                &request_header_decisions,
                body,
                metrics,
                deadline,
                shutdown,
                cache_transaction,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn dispatch_h3_upstream_request<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    authority: &Authority,
    proxy: &ProxyActionPlan,
    request_header_decisions: &[RequestHeaderDecision],
    body: Bytes,
    metrics: &ListenerMetrics,
    deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
    mut cache_transaction: Option<CacheTransaction>,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let Some(h3) = proxy.pool.h3() else {
        return send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream policy is unavailable\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_GATEWAY.as_u16());
    };
    let Some(server_name) = h3.server_name() else {
        return send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream SNI is unavailable\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_GATEWAY.as_u16());
    };
    let mut attempted = Vec::new();
    let mut retry_server: Option<String> = None;
    let max_attempts = usize::from(proxy.policy.max_retries).saturating_add(1);
    let retry_safe = matches!(request.method(), &Method::GET | &Method::HEAD) && body.is_empty();
    let mut last_error = H3UpstreamError::Connect;

    for attempt in 0..max_attempts {
        let mut selected = if let Some(server) = retry_server.take() {
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    return None;
                }
                result = timeout_at(deadline, proxy.pool.select_server_endpoint(&server)) => {
                    match result {
                        Ok(Ok(selected)) => selected,
                        Ok(Err(_)) => break,
                        Err(_) => {
                            last_error = H3UpstreamError::Timeout;
                            break;
                        }
                    }
                }
            }
        } else {
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    return None;
                }
                result = timeout_at(deadline, proxy.pool.select_endpoint(&attempted)) => {
                    match result {
                        Ok(Ok(selected)) => selected,
                        Ok(Err(_)) => break,
                        Err(_) => {
                            last_error = H3UpstreamError::Timeout;
                            break;
                        }
                    }
                }
            }
        };
        let Ok(Some(selected_host)) = selected_upstream_host(
            selected.endpoint(),
            proxy.policy.upstream_host.clone(),
            Some(authority),
        ) else {
            last_error = H3UpstreamError::Protocol;
            break;
        };
        let Ok(mut upstream_request) = build_h3_upstream_request(
            request,
            body.len(),
            &selected_host,
            &proxy.policy,
            request_header_decisions,
        ) else {
            last_error = H3UpstreamError::Protocol;
            break;
        };
        if let Some(transaction) = cache_transaction.as_ref() {
            if transaction.request().method == Method::HEAD {
                *upstream_request.method_mut() = Method::GET;
            }
            transaction.apply_validators(upstream_request.headers_mut());
        }
        attempted.push(selected.server_name().to_owned());
        let mut response = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                last_error = H3UpstreamError::Timeout;
                break;
            }
            let Ok(address) = selected.prepare_h3_address(remaining).await else {
                last_error = H3UpstreamError::Connect;
                break;
            };
            match h3
                .request(
                    address,
                    server_name,
                    upstream_request.clone(),
                    body.clone(),
                    deadline,
                    shutdown.clone(),
                )
                .await
            {
                Ok(value) => {
                    response = Some(value);
                    break;
                }
                Err(error) if error.retryable() && selected.has_address_fallback() => {
                    last_error = error;
                }
                Err(error) => {
                    last_error = error;
                    break;
                }
            }
        }
        if let Some(response) = response {
            if proxy.policy.retries_on_status(response.status.as_u16()) {
                selected
                    .observation()
                    .record_passive_failure(HealthFailure::UnexpectedStatus);
                let retry_target = proxy.policy.target_for_retry(attempted.len());
                let target_available = match retry_target {
                    HttpRetryTarget::SameServer => attempted.last().is_some(),
                    HttpRetryTarget::NextServer => proxy.pool.has_unattempted(&attempted),
                };
                let retry = retry_safe
                    && attempt.saturating_add(1) < max_attempts
                    && target_available
                    && Instant::now() < deadline;
                if retry {
                    let delay = proxy.policy.retry_delay;
                    if !delay.is_zero() {
                        if delay > deadline.saturating_duration_since(Instant::now()) {
                            return send_h3_upstream_response(
                                stream,
                                request,
                                response,
                                proxy,
                                metrics,
                                deadline,
                                &mut cache_transaction,
                            )
                            .await;
                        }
                        tokio::select! {
                            changed = shutdown.changed() => {
                                let _ = changed;
                                return None;
                            }
                            () = tokio::time::sleep(delay) => {}
                        }
                    }
                    if retry_target == HttpRetryTarget::SameServer {
                        retry_server = attempted.last().cloned();
                    }
                    continue;
                }
            }
            return send_h3_upstream_response(
                stream,
                request,
                response,
                proxy,
                metrics,
                deadline,
                &mut cache_transaction,
            )
            .await;
        }

        if let Some(failure) = h3_passive_failure(&last_error) {
            selected.observation().record_passive_failure(failure);
        }

        let trigger = h3_retry_trigger(&last_error);
        let retry_target = proxy.policy.target_for_retry(attempted.len());
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => attempted.last().is_some(),
            HttpRetryTarget::NextServer => proxy.pool.has_unattempted(&attempted),
        };
        let retry = retry_safe
            && attempt.saturating_add(1) < max_attempts
            && target_available
            && proxy.policy.retries_on(trigger)
            && last_error.retryable()
            && Instant::now() < deadline;
        if !retry {
            break;
        }
        let delay = proxy.policy.retry_delay;
        if !delay.is_zero() {
            if delay > deadline.saturating_duration_since(Instant::now()) {
                break;
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    return None;
                }
                () = tokio::time::sleep(delay) => {}
            }
        }
        if retry_target == HttpRetryTarget::SameServer {
            retry_server = attempted.last().cloned();
        }
    }

    let status = if matches!(last_error, H3UpstreamError::Timeout) {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::BAD_GATEWAY
    };
    if let Some(response) = stale_h3_response(&mut cache_transaction).await {
        let status = conditional_h3_cache_status(request, &response);
        return send_h3_cached_response(stream, request, response, deadline)
            .await
            .then_some(status.as_u16());
    }
    if let Some(transaction) = cache_transaction.as_mut() {
        transaction.complete_without_store();
    }
    send_h3_error(
        stream,
        status,
        b"HTTP/3 upstream request failed\n",
        deadline,
    )
    .await
    .then_some(status.as_u16())
}

fn h3_retry_trigger(error: &H3UpstreamError) -> HttpRetryTrigger {
    match error {
        H3UpstreamError::Timeout => HttpRetryTrigger::ConnectTimeout,
        H3UpstreamError::RefusedStream => HttpRetryTrigger::RefusedStream,
        H3UpstreamError::Connect
        | H3UpstreamError::Protocol
        | H3UpstreamError::Cancelled
        | H3UpstreamError::ResponseBodyTooLarge
        | H3UpstreamError::RequestBodyTooLarge
        | H3UpstreamError::ResourceExhausted
        | H3UpstreamError::MissingServerName => HttpRetryTrigger::ConnectFailure,
    }
}

fn h3_passive_failure(error: &H3UpstreamError) -> Option<HealthFailure> {
    match error {
        H3UpstreamError::Connect => Some(HealthFailure::ConnectFailed),
        H3UpstreamError::Timeout => Some(HealthFailure::Timeout),
        H3UpstreamError::RefusedStream | H3UpstreamError::Protocol => {
            Some(HealthFailure::ProtocolError)
        }
        H3UpstreamError::Cancelled
        | H3UpstreamError::ResponseBodyTooLarge
        | H3UpstreamError::RequestBodyTooLarge
        | H3UpstreamError::ResourceExhausted
        | H3UpstreamError::MissingServerName => None,
    }
}

#[allow(clippy::too_many_lines)]
async fn send_h3_upstream_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    response: crate::http3_upstream::H3UpstreamResponse,
    proxy: &ProxyActionPlan,
    metrics: &ListenerMetrics,
    deadline: Instant,
    cache_transaction: &mut Option<CacheTransaction>,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let status = response.status;
    let mut headers = response.headers;
    let body = response.body;
    let mut trailers = response.trailers;
    if apply_response_policy_map(status, &mut headers, &proxy.policy).is_err() {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        let _ = send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream response headers are invalid\n",
            deadline,
        )
        .await;
        return Some(StatusCode::BAD_GATEWAY.as_u16());
    }
    let body_length = u64::try_from(body.len()).unwrap_or(u64::MAX);
    if sanitize_h3_response_headers(
        &mut headers,
        status,
        body_length,
        request.method() == Method::HEAD,
    )
    .is_err()
    {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        let _ = send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream response cannot be represented safely\n",
            deadline,
        )
        .await;
        return Some(StatusCode::BAD_GATEWAY.as_u16());
    }

    if cache_transaction
        .as_ref()
        .is_some_and(CacheTransaction::is_revalidation)
    {
        if status == StatusCode::NOT_MODIFIED {
            let transaction = cache_transaction
                .as_mut()
                .expect("revalidation has a cache transaction");
            let timing = ResponseTiming {
                request_started: transaction.request().request_started,
                response_received: transaction.now(),
                response_received_wall: std::time::SystemTime::now(),
            };
            let cached = transaction.finish_revalidation(&headers, timing).await;
            let cached_status = conditional_h3_cache_status(request, &cached);
            return send_h3_cached_response(stream, request, cached, deadline)
                .await
                .then_some(cached_status.as_u16());
        }
        if status.is_server_error()
            && let Some(cached) = stale_h3_response(cache_transaction).await
        {
            let cached_status = conditional_h3_cache_status(request, &cached);
            return send_h3_cached_response(stream, request, cached, deadline)
                .await
                .then_some(cached_status.as_u16());
        }
    }

    let mut prepared = None;
    if let Some(transaction) = cache_transaction.as_mut() {
        let tags = transaction
            .surrogate_header()
            .map_or_else(Vec::new, |header| h3_surrogate_tags(&headers, header));
        let tags_valid = transaction.cache_tags_within_limits(&tags);
        if trailers.as_ref().is_none_or(HeaderMap::is_empty)
            && tags_valid
            && h3_cache_representation_valid(status, &headers, body.len())
        {
            let tag_refs = tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
            let timing = ResponseTiming {
                request_started: transaction.request().request_started,
                response_received: transaction.now(),
                response_received_wall: std::time::SystemTime::now(),
            };
            prepared = transaction
                .prepare_response(status, &headers, body.clone(), timing, &tag_refs)
                .ok();
        }
        if prepared.is_none() {
            transaction.complete_without_store();
        }
    }
    let mut response_head = Response::new(());
    *response_head.status_mut() = status;
    *response_head.headers_mut() = headers;
    if !matches!(
        timeout_at(deadline, stream.send_response(response_head)).await,
        Ok(Ok(()))
    ) {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.cancel();
        }
        return None;
    }
    let body_forbidden = matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    if request.method() != Method::HEAD && !body_forbidden && !body.is_empty() {
        let _ = metrics.record_bytes_sent(body_length);
        if !matches!(
            timeout_at(deadline, stream.send_data(body)).await,
            Ok(Ok(()))
        ) {
            if let Some(transaction) = cache_transaction.as_mut() {
                transaction.cancel();
            }
            return None;
        }
    }
    if request.method() != Method::HEAD
        && let Some(mut trailers) = trailers.take()
        && (sanitize_h3_trailers(&mut trailers).is_err()
            || !matches!(
                timeout_at(deadline, stream.send_trailers(trailers)).await,
                Ok(Ok(()))
            ))
    {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.cancel();
        }
        return None;
    }
    if !matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(()))) {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.cancel();
        }
        return None;
    }
    if let Some(entry) = prepared
        && let Some(transaction) = cache_transaction.as_mut()
    {
        let _ = transaction.admit(entry).await;
    }
    Some(status.as_u16())
}

fn build_h3_upstream_request(
    request: &Request<()>,
    body_length: usize,
    selected_host: &HeaderValue,
    policy: &ProxyPolicyPlan,
    request_header_decisions: &[RequestHeaderDecision],
) -> Result<Request<()>, ()> {
    let mut uri = request.uri().clone();
    if let Some(rewrite) = &policy.upstream_path_rewrite {
        rewrite_upstream_path(&mut uri, rewrite).map_err(|_| ())?;
    }
    let selected_authority = selected_host
        .to_str()
        .map_err(|_| ())?
        .parse::<Authority>()
        .map_err(|_| ())?;
    let path = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    let uri = Uri::builder()
        .scheme("https")
        .authority(selected_authority.as_str())
        .path_and_query(path)
        .build()
        .map_err(|_| ())?;
    let mut headers = request.headers().clone();
    for name in [
        HOST,
        CONNECTION,
        KEEP_ALIVE,
        PROXY_CONNECTION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(name);
    }
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&body_length.to_string()).map_err(|_| ())?,
    );
    apply_h3_header_mutations(&mut headers, request_header_decisions, selected_host)?;
    let mut output = Request::builder()
        .method(request.method().clone())
        .uri(uri)
        .body(())
        .map_err(|_| ())?;
    *output.version_mut() = http::Version::HTTP_3;
    *output.headers_mut() = headers;
    Ok(output)
}

fn apply_h3_header_mutations(
    headers: &mut HeaderMap,
    decisions: &[RequestHeaderDecision],
    selected_upstream_host: &HeaderValue,
) -> Result<(), ()> {
    for decision in decisions {
        match decision {
            RequestHeaderDecision::Remove(name) => {
                headers.remove(name);
            }
            RequestHeaderDecision::Set { name, value } => {
                let Some(value) = value
                    .complete(Some(selected_upstream_host))
                    .map_err(|_| ())?
                else {
                    continue;
                };
                headers.insert(name.clone(), value);
            }
        }
    }
    Ok(())
}

fn h3_request_header_decisions(
    request: &Request<()>,
    authority: &Authority,
    policy: &ProxyPolicyPlan,
    client_addr: Option<SocketAddr>,
) -> Result<Box<[RequestHeaderDecision]>, RequestPolicyError> {
    let context = RequestPolicyContext {
        authority: Some(authority),
        downstream_scheme: "https",
        client_ip: client_addr.map(|address| address.ip()),
        incoming_headers: request.headers(),
    };
    let mut decisions = Vec::with_capacity(policy.request_headers.len());
    for mutation in &policy.request_headers {
        if mutation.is_pingora_managed_upgrade() {
            continue;
        }
        decisions.push(decide_request_header(mutation, context)?);
    }
    Ok(decisions.into_boxed_slice())
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

pub(crate) fn request_content_length(headers: &HeaderMap) -> Result<Option<u64>, ()> {
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

enum H3StaticErrorAction {
    File {
        file: StaticFile,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    InternalRedirect {
        path: String,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    Literal {
        body: Bytes,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    Empty,
}

async fn h3_static_error_action(
    files: &crate::http_action::StaticFilesPlan,
    status: u16,
) -> H3StaticErrorAction {
    match files.error_document(status).await {
        Some(StaticErrorTarget::File { file, headers }) => {
            H3StaticErrorAction::File { file, headers }
        }
        Some(StaticErrorTarget::InternalRedirect { path, headers }) => {
            H3StaticErrorAction::InternalRedirect { path, headers }
        }
        Some(StaticErrorTarget::Literal { body, headers }) => {
            H3StaticErrorAction::Literal { body, headers }
        }
        None => H3StaticErrorAction::Empty,
    }
}

#[allow(clippy::too_many_lines)]
async fn send_h3_static_request<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    files: &crate::http_action::StaticFilesPlan,
    request: &Request<()>,
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        let headers = [(
            HeaderName::from_static("allow"),
            HeaderValue::from_static("GET, HEAD"),
        )];
        return send_h3_response(
            stream,
            StatusCode::METHOD_NOT_ALLOWED,
            &headers,
            Bytes::new(),
            false,
            deadline,
        )
        .await
        .then_some(StatusCode::METHOD_NOT_ALLOWED.as_u16());
    }

    let mut path = request.uri().path().to_owned();
    let mut status_override = None;
    let mut pending_headers = Vec::new();
    for _ in 0..=MAX_STATIC_REDIRECTS {
        match files.serve(&path).await {
            Ok(StaticTarget::File(file)) => {
                let status = status_override.unwrap_or(200);
                let range = if status == 200 {
                    match files.request_decision(request.headers(), &file) {
                        StaticRequestDecision::NotModified => {
                            let mut headers = files.headers(304);
                            headers.extend(h3_static_validator_headers(files, &file));
                            return send_h3_response(
                                stream,
                                StatusCode::NOT_MODIFIED,
                                &headers,
                                Bytes::new(),
                                true,
                                deadline,
                            )
                            .await
                            .then_some(StatusCode::NOT_MODIFIED.as_u16());
                        }
                        StaticRequestDecision::PreconditionFailed => {
                            let mut headers = files.headers(412);
                            headers.extend(h3_static_validator_headers(files, &file));
                            return send_h3_response(
                                stream,
                                StatusCode::PRECONDITION_FAILED,
                                &headers,
                                Bytes::new(),
                                head,
                                deadline,
                            )
                            .await
                            .then_some(StatusCode::PRECONDITION_FAILED.as_u16());
                        }
                        StaticRequestDecision::RangeNotSatisfiable => {
                            let headers = [
                                (
                                    HeaderName::from_static("accept-ranges"),
                                    HeaderValue::from_static("bytes"),
                                ),
                                (
                                    HeaderName::from_static("content-range"),
                                    HeaderValue::from_str(&format!("bytes */{}", file.size))
                                        .expect("static size is a valid header value"),
                                ),
                            ];
                            let _ = send_h3_response(
                                stream,
                                StatusCode::RANGE_NOT_SATISFIABLE,
                                &headers,
                                Bytes::new(),
                                head,
                                deadline,
                            )
                            .await;
                            return Some(StatusCode::RANGE_NOT_SATISFIABLE.as_u16());
                        }
                        StaticRequestDecision::Serve { range } => range,
                    }
                } else {
                    None
                };
                return send_h3_static_file(
                    stream,
                    files,
                    file,
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    range,
                    head,
                    &pending_headers,
                    deadline,
                )
                .await;
            }
            Ok(StaticTarget::Autoindex { body }) => {
                let status = status_override.take().unwrap_or(200);
                let mut headers = files.headers(status);
                headers.extend(std::mem::take(&mut pending_headers));
                headers.push((
                    HeaderName::from_static("content-type"),
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ));
                return send_h3_response(
                    stream,
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    &headers,
                    body,
                    head,
                    deadline,
                )
                .await
                .then_some(status);
            }
            Ok(StaticTarget::DirectoryRedirect { path: redirect }) => {
                let Ok(location) = HeaderValue::from_str(&redirect) else {
                    return send_h3_error(
                        stream,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        b"static directory redirect is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::INTERNAL_SERVER_ERROR.as_u16());
                };
                let mut headers = files.headers(301);
                headers.extend(std::mem::take(&mut pending_headers));
                headers.push((HeaderName::from_static("location"), location));
                return send_h3_response(
                    stream,
                    StatusCode::MOVED_PERMANENTLY,
                    &headers,
                    Bytes::new(),
                    head,
                    deadline,
                )
                .await
                .then_some(StatusCode::MOVED_PERMANENTLY.as_u16());
            }
            Ok(StaticTarget::InternalRedirect { path: redirect }) => {
                path = redirect;
            }
            Ok(StaticTarget::Status(status)) => {
                if let Some(next) = apply_h3_static_error(
                    stream,
                    files,
                    status,
                    head,
                    &mut path,
                    &mut status_override,
                    &mut pending_headers,
                    deadline,
                )
                .await
                {
                    return Some(next);
                }
            }
            Err(error) => {
                let status = match error {
                    StaticServeError::Unsafe => 403,
                    StaticServeError::NotFound => 404,
                    StaticServeError::TooLarge | StaticServeError::Unavailable => 500,
                };
                if let Some(next) = apply_h3_static_error(
                    stream,
                    files,
                    status,
                    head,
                    &mut path,
                    &mut status_override,
                    &mut pending_headers,
                    deadline,
                )
                .await
                {
                    return Some(next);
                }
            }
        }
    }
    send_h3_error(
        stream,
        StatusCode::INTERNAL_SERVER_ERROR,
        b"static internal redirect limit exceeded\n",
        deadline,
    )
    .await
    .then_some(StatusCode::INTERNAL_SERVER_ERROR.as_u16())
}

#[allow(clippy::too_many_arguments)]
async fn apply_h3_static_error<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    files: &crate::http_action::StaticFilesPlan,
    status: u16,
    head: bool,
    path: &mut String,
    status_override: &mut Option<u16>,
    pending_headers: &mut Vec<(HeaderName, HeaderValue)>,
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    match h3_static_error_action(files, status).await {
        H3StaticErrorAction::File { file, headers } => {
            let mut all_headers = headers.into_vec();
            all_headers.extend(std::mem::take(pending_headers));
            send_h3_static_file(
                stream,
                files,
                file,
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                None,
                head,
                &all_headers,
                deadline,
            )
            .await
        }
        H3StaticErrorAction::InternalRedirect {
            path: redirect,
            headers,
        } => {
            *path = redirect;
            *status_override = Some(status);
            pending_headers.extend(headers.into_vec());
            None
        }
        H3StaticErrorAction::Literal { body, headers } => {
            let mut all_headers = files.headers(status);
            all_headers.extend(headers.into_vec());
            all_headers.extend(std::mem::take(pending_headers));
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let _ = send_h3_response(stream, status, &all_headers, body, head, deadline).await;
            Some(status.as_u16())
        }
        H3StaticErrorAction::Empty => {
            let mut all_headers = files.headers(status);
            all_headers.extend(std::mem::take(pending_headers));
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let _ =
                send_h3_response(stream, status, &all_headers, Bytes::new(), head, deadline).await;
            Some(status.as_u16())
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn send_h3_static_file<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    files: &crate::http_action::StaticFilesPlan,
    file: StaticFile,
    status: StatusCode,
    range: Option<(u64, u64)>,
    head: bool,
    extra_headers: &[(HeaderName, HeaderValue)],
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let status = if range.is_some() && status == StatusCode::OK {
        StatusCode::PARTIAL_CONTENT
    } else {
        status
    };
    let (start, end) = range.unwrap_or_else(|| (0, file.size.saturating_sub(1)));
    let length = if file.size == 0 { 0 } else { end - start + 1 };
    if length > H3_MAX_RESPONSE_BODY_BYTES {
        let _ = send_h3_error(
            stream,
            StatusCode::PAYLOAD_TOO_LARGE,
            b"static response body exceeds the HTTP/3 limit\n",
            deadline,
        )
        .await;
        return Some(StatusCode::PAYLOAD_TOO_LARGE.as_u16());
    }

    let mut headers = files.headers(status.as_u16());
    headers.extend_from_slice(extra_headers);
    headers.extend(h3_static_validator_headers(files, &file));
    headers.push((
        HeaderName::from_static("content-type"),
        files.content_type(&file.name),
    ));
    headers.push((
        HeaderName::from_static("accept-ranges"),
        HeaderValue::from_static("bytes"),
    ));
    headers.push((
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).expect("static length is a valid header"),
    ));
    if range.is_some() {
        headers.push((
            HeaderName::from_static("content-range"),
            HeaderValue::from_str(&format!("bytes {start}-{end}/{}", file.size))
                .expect("static range is a valid header"),
        ));
    }
    let status_code = status.as_u16();
    let mut response = Response::new(());
    *response.status_mut() = status;
    for (name, value) in headers {
        response.headers_mut().append(name, value);
    }
    let mut response_headers = response.headers().clone();
    if sanitize_h3_response_headers(&mut response_headers, status, length, head).is_err() {
        let _ = send_h3_error(
            stream,
            StatusCode::INTERNAL_SERVER_ERROR,
            b"static response headers are invalid\n",
            deadline,
        )
        .await;
        return Some(StatusCode::INTERNAL_SERVER_ERROR.as_u16());
    }
    *response.headers_mut() = response_headers;
    if !matches!(
        timeout_at(deadline, stream.send_response(response)).await,
        Ok(Ok(()))
    ) {
        return None;
    }
    if head || length == 0 {
        if !matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(()))) {
            return None;
        }
        return Some(status_code);
    }

    let mut file = TokioFile::from_std(file.file);
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return None;
    }
    let mut remaining = length;
    let mut buffer = vec![0; 64 * 1024];
    while remaining != 0 {
        let chunk_size = usize::try_from(remaining.min(buffer.len() as u64)).expect("chunk bound");
        let read = match timeout_at(deadline, file.read(&mut buffer[..chunk_size])).await {
            Ok(Ok(read)) if read != 0 => read,
            _ => return None,
        };
        remaining -= u64::try_from(read).expect("read length fits u64");
        if !matches!(
            timeout_at(
                deadline,
                stream.send_data(Bytes::copy_from_slice(&buffer[..read])),
            )
            .await,
            Ok(Ok(()))
        ) {
            return None;
        }
    }
    if !matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(()))) {
        return None;
    }
    Some(status_code)
}

fn h3_static_validator_headers(
    files: &crate::http_action::StaticFilesPlan,
    file: &StaticFile,
) -> Vec<(HeaderName, HeaderValue)> {
    let mut headers = Vec::with_capacity(usize::from(files.etag_enabled()) + 1);
    if files.etag_enabled() {
        headers.push((HeaderName::from_static("etag"), file.etag.clone()));
    }
    headers.push((
        HeaderName::from_static("last-modified"),
        HeaderValue::from_str(&httpdate::fmt_http_date(file.modified))
            .expect("HTTP date is a valid header value"),
    ));
    headers
}

fn strip_h3_hop_by_hop_headers(headers: &mut HeaderMap) -> Result<(), ()> {
    let mut nominated = Vec::new();
    for value in headers.get_all(CONNECTION) {
        for token in value.to_str().map_err(|_| ())?.split(',') {
            nominated.push(HeaderName::from_bytes(token.trim().as_bytes()).map_err(|_| ())?);
        }
    }
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        CONNECTION,
        KEEP_ALIVE,
        HeaderName::from_static("proxy-authenticate"),
        HeaderName::from_static("proxy-authorization"),
        PROXY_CONNECTION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(name);
    }
    Ok(())
}

pub(crate) fn sanitize_h3_response_headers(
    headers: &mut HeaderMap,
    status: StatusCode,
    body_length: u64,
    head: bool,
) -> Result<(), ()> {
    if status.is_informational() || status == StatusCode::SWITCHING_PROTOCOLS {
        return Err(());
    }
    let declared_length = request_content_length(headers)?;
    strip_h3_hop_by_hop_headers(headers)?;
    let body_forbidden = matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let representation_length = if head {
        declared_length.unwrap_or(body_length)
    } else {
        body_length
    };
    if representation_length > H3_MAX_RESPONSE_BODY_BYTES {
        return Err(());
    }
    if body_forbidden {
        headers.remove(CONTENT_LENGTH);
    } else {
        headers.remove(CONTENT_LENGTH);
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&representation_length.to_string()).map_err(|_| ())?,
        );
    }
    if !head && body_forbidden && body_length != 0 {
        return Err(());
    }
    (header_bytes(headers) <= usize::try_from(H3_MAX_FIELD_SECTION_BYTES).map_err(|_| ())?)
        .then_some(())
        .ok_or(())
}

pub(crate) fn sanitize_h3_trailers(headers: &mut HeaderMap) -> Result<(), ()> {
    strip_h3_hop_by_hop_headers(headers)?;
    headers.remove(CONTENT_LENGTH);
    (header_bytes(headers) <= usize::try_from(H3_MAX_FIELD_SECTION_BYTES).map_err(|_| ())?)
        .then_some(())
        .ok_or(())
}

async fn start_h3_cache(
    cache: &Arc<HttpCachePlan>,
    request: &Request<()>,
    authority: &Authority,
    metrics: &ListenerMetrics,
) -> H3CacheDecision {
    let request = CacheRequest {
        method: request.method().clone(),
        scheme: "https",
        authority: authority.as_str().to_owned(),
        path: request.uri().path().to_owned(),
        query: request.uri().query().map(str::to_owned),
        headers: request.headers().clone(),
        request_started: cache.cache.now(),
    };
    match CacheTransaction::new(Arc::clone(cache), request, metrics.clone())
        .start()
        .await
    {
        CacheStart::Bypass(failure) => {
            if let CacheStartFailure::Lookup(error) = failure
                && !error.is_invalid_request()
            {
                warn!("HTTP/3 cache lookup bypassed after validation failure: {error}");
            }
            H3CacheDecision::Bypass
        }
        CacheStart::Hit(response) => H3CacheDecision::Respond(Box::new(response)),
        CacheStart::OnlyIfCached => H3CacheDecision::OnlyIfCached,
        CacheStart::MissLeader(transaction) | CacheStart::RevalidationLeader(transaction) => {
            H3CacheDecision::Continue(Box::new(transaction))
        }
    }
}

async fn stale_h3_response(
    cache_transaction: &mut Option<CacheTransaction>,
) -> Option<CachedResponse> {
    let transaction = cache_transaction.as_mut()?;
    let response = transaction
        .stale_response(CacheFailureClass::Upstream)
        .await?;
    transaction.complete_without_store();
    transaction.record_hit();
    Some(response)
}

async fn purge_h3_cache<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    authority: &Authority,
    cache: Arc<HttpCachePlan>,
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let access = cache
        .purge_access
        .as_ref()
        .expect("purge dispatch requires access policy");
    if !access.authorizes(request.headers()) {
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
    let cache_request = CacheRequest {
        method: request.method().clone(),
        scheme: "https",
        authority: authority.as_str().to_owned(),
        path: request.uri().path().to_owned(),
        query: request.uri().query().map(str::to_owned),
        headers: request.headers().clone(),
        request_started: cache.cache.now(),
    };
    let result = if let Some(value) = cache
        .surrogate_header
        .as_ref()
        .and_then(|header| request.headers().get(header))
    {
        cache.cache.purge_tag(value.as_bytes()).await
    } else {
        match cache.cache.base(&cache_request) {
            Ok(base) => cache.cache.purge_base(&base).await,
            Err(error) => Err(error),
        }
    };
    let status = match result {
        Ok(result) if result.entries == 0 => StatusCode::NOT_FOUND,
        Ok(_) => StatusCode::OK,
        Err(error) => {
            warn!("HTTP/3 cache purge failed: {error}");
            if error.is_invalid_request() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    };
    let headers = [(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    )];
    send_h3_response(stream, status, &headers, Bytes::new(), false, deadline)
        .await
        .then_some(status.as_u16())
}

async fn send_h3_cached_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    mut response: CachedResponse,
    deadline: Instant,
) -> bool
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    if conditional_h3_cache_status(request, &response) == StatusCode::NOT_MODIFIED {
        response.status = StatusCode::NOT_MODIFIED;
        response.body = Bytes::new();
    }
    let headers = response
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    send_h3_response(
        stream,
        response.status,
        &headers,
        response.body,
        request.method() == Method::HEAD,
        deadline,
    )
    .await
}

fn conditional_h3_cache_status(request: &Request<()>, response: &CachedResponse) -> StatusCode {
    if h3_cached_response_matches_condition(request, response) {
        StatusCode::NOT_MODIFIED
    } else {
        response.status
    }
}

fn h3_cached_response_matches_condition(request: &Request<()>, response: &CachedResponse) -> bool {
    if let Some(if_none_match) = request.headers().get(http::header::IF_NONE_MATCH) {
        return if_none_match
            .as_bytes()
            .split(|byte| *byte == b',')
            .map(trim_h3_ows)
            .any(|candidate| {
                candidate == b"*"
                    || response
                        .headers
                        .get(http::header::ETAG)
                        .is_some_and(|current| {
                            strip_h3_weak_etag(candidate) == strip_h3_weak_etag(current.as_bytes())
                        })
            });
    }
    if !matches!(request.method(), &Method::GET | &Method::HEAD) {
        return false;
    }
    let Some(if_modified_since) = request.headers().get(http::header::IF_MODIFIED_SINCE) else {
        return false;
    };
    let Some(last_modified) = response.headers.get(http::header::LAST_MODIFIED) else {
        return false;
    };
    let (Ok(if_modified_since), Ok(last_modified)) =
        (if_modified_since.to_str(), last_modified.to_str())
    else {
        return false;
    };
    let (Ok(if_modified_since), Ok(last_modified)) = (
        httpdate::parse_http_date(if_modified_since),
        httpdate::parse_http_date(last_modified),
    ) else {
        return false;
    };
    last_modified <= if_modified_since
}

fn trim_h3_ows(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(start, |index| index + 1);
    &value[start..end]
}

fn strip_h3_weak_etag(value: &[u8]) -> &[u8] {
    let value = trim_h3_ows(value);
    value
        .strip_prefix(b"W/")
        .or_else(|| value.strip_prefix(b"w/"))
        .unwrap_or(value)
}

fn h3_cache_representation_valid(status: StatusCode, headers: &HeaderMap, body_len: usize) -> bool {
    if status.is_informational()
        || matches!(
            status,
            StatusCode::SWITCHING_PROTOCOLS | StatusCode::NOT_MODIFIED
        )
    {
        return false;
    }
    if matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT) && body_len != 0 {
        return false;
    }
    request_content_length(headers)
        .ok()
        .flatten()
        .is_none_or(|length| length == u64::try_from(body_len).unwrap_or(u64::MAX))
}

fn h3_surrogate_tags(headers: &HeaderMap, name: &HeaderName) -> Vec<Bytes> {
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
    let body_length = u64::try_from(body.len()).map_err(|_| ()).ok();
    let Some(body_length) = body_length else {
        return false;
    };
    let Ok(mut response) = Response::builder().status(status).body(()) else {
        return false;
    };
    for (name, value) in headers {
        response.headers_mut().append(name.clone(), value.clone());
    }
    if sanitize_h3_response_headers(response.headers_mut(), status, body_length, head).is_err() {
        return false;
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
    location: &RedirectLocationPlan,
    authority: &Authority,
    uri: &Uri,
) -> Option<HeaderValue> {
    let host = normalized_redirect_host(authority);
    let request_uri = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    expand_redirect_location(
        location,
        RedirectContext {
            scheme: "https",
            normalized_host: Some(&host),
            request_uri,
        },
    )
}

fn unix_time_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

pub(crate) fn capability_snapshot(
    listeners: &[crate::ListenerSnapshot],
    mode: RuntimeMode,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "supervision": {
            "mode": mode,
            "descriptorAdoption": {
                "status": if mode == RuntimeMode::Supervised { "negotiated" } else { "not_used" },
                "manifestVersion": 1,
                "datagram": true,
                "quic": true,
            },
        },
        "udp": udp_listener_capability(listeners),
        "http3": {
            "reverse": listener_capability(listeners, "http3"),
            "forward": listener_capability(listeners, "forward_http3"),
        },
    })
}

fn udp_listener_capability(listeners: &[crate::ListenerSnapshot]) -> serde_json::Value {
    let configured = listeners
        .iter()
        .filter(|listener| listener.protocol == "udp")
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
    let blocked_reason = if status != "blocked" {
        None
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Failed)
    {
        Some("listener_runtime_failed")
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Stopped)
    {
        Some("listener_stopped")
    } else {
        Some("listener_not_listening")
    };
    serde_json::json!({
        "status": status,
        "supported": true,
        "listeners": active,
        "configuredListeners": configured.iter().map(|listener| listener.name.clone()).collect::<Vec<_>>(),
        "transport": "udp",
        "drain": "graceful",
        "fallback": "none",
        "blockedReason": blocked_reason,
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
    let blocked_reason = if status != "blocked" {
        None
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Failed)
    {
        Some("listener_runtime_failed")
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Stopped)
    {
        Some("listener_stopped")
    } else {
        Some("listener_not_listening")
    };
    serde_json::json!({
        "status": status,
        "supported": true,
        "listeners": active,
        "configuredListeners": configured.iter().map(|listener| listener.name.clone()).collect::<Vec<_>>(),
        "transport": "quic",
        "alpn": ["h3"],
        "tlsMinVersion": "1.3",
        "zeroRtt": "disabled",
        "migration": "disabled",
        "goAway": "graceful",
        "fallback": "none",
        "unsupported": ["compression", "upgrades"],
        "limits": {
            "maxHandshakesAndConnections": H3_HANDSHAKE_LIMIT,
            "maxBidirectionalStreams": H3_BIDI_STREAM_LIMIT,
            "maxUnidirectionalStreams": H3_UNI_STREAM_LIMIT,
            "maxFieldSectionBytes": H3_MAX_FIELD_SECTION_BYTES,
            "maxRequestBodyBytes": H3_MAX_REQUEST_BODY_BYTES,
            "maxResponseBodyBytes": H3_MAX_RESPONSE_BODY_BYTES,
        },
        "blockedReason": blocked_reason,
    })
}

async fn handle_request<C>(
    resolver: RequestResolver<C, bytes::Bytes>,
    service: Arc<ForwardHttp1ServicePlan>,
    client_addr: Option<std::net::SocketAddr>,
    shutdown: watch::Receiver<bool>,
    metrics: ListenerMetrics,
) where
    C: h3::quic::Connection<bytes::Bytes> + Send + 'static,
    C::BidiStream: h3::quic::BidiStream<bytes::Bytes> + Send + 'static,
{
    let Ok((request, stream)) = resolver.resolve_request().await else {
        return;
    };
    service
        .handle_h3(request, stream, client_addr, shutdown, metrics)
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
    use oxiroute_config::{
        HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    };

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
        let value = capability_snapshot(&listeners, RuntimeMode::Direct);

        assert_eq!(value["http3"]["reverse"]["status"], "active");
        assert_eq!(value["http3"]["forward"]["status"], "unconfigured");
        assert_eq!(value["udp"]["status"], "unconfigured");
        assert_eq!(value["supervision"]["mode"], "direct");
        assert_eq!(value["supervision"]["descriptorAdoption"]["datagram"], true);
        assert_eq!(value["supervision"]["descriptorAdoption"]["quic"], true);
        assert_eq!(value["http3"]["reverse"]["supported"], true);
        assert_eq!(value["http3"]["reverse"]["goAway"], "graceful");
        assert_eq!(value["http3"]["reverse"]["fallback"], "none");
        assert_eq!(
            value["http3"]["reverse"]["unsupported"],
            serde_json::json!(["compression", "upgrades"])
        );
        assert_eq!(
            value["http3"]["reverse"]["limits"]["maxRequestBodyBytes"],
            H3_MAX_REQUEST_BODY_BYTES
        );
    }

    #[test]
    fn exposes_the_bounded_h3_quic_and_message_contract() {
        let value = capability_snapshot(&[], RuntimeMode::Direct);
        let limits = &value["http3"]["reverse"]["limits"];

        assert_eq!(limits["maxHandshakesAndConnections"], H3_HANDSHAKE_LIMIT);
        assert_eq!(limits["maxBidirectionalStreams"], H3_BIDI_STREAM_LIMIT);
        assert_eq!(limits["maxUnidirectionalStreams"], H3_UNI_STREAM_LIMIT);
        assert_eq!(limits["maxFieldSectionBytes"], H3_MAX_FIELD_SECTION_BYTES);
        assert_eq!(limits["maxRequestBodyBytes"], H3_MAX_REQUEST_BODY_BYTES);
        assert_eq!(limits["maxResponseBodyBytes"], H3_MAX_RESPONSE_BODY_BYTES);
    }

    #[test]
    fn reports_forward_h3_cache_as_supported() {
        let value = capability_snapshot(&[], RuntimeMode::Direct);

        assert_eq!(
            value["http3"]["forward"]["unsupported"],
            serde_json::json!(["compression", "upgrades"])
        );
    }

    #[tokio::test]
    async fn h3_drain_waits_for_admitted_requests_to_finish() {
        let (request_cancel, cancelled) = watch::channel(false);
        let mut requests = tokio::task::JoinSet::new();
        requests.spawn(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
        });

        join_h3_requests(
            "test HTTP/3",
            &mut requests,
            &request_cancel,
            Instant::now() + Duration::from_secs(1),
        )
        .await;

        assert!(!*cancelled.borrow(), "completed request was cancelled");
    }

    #[tokio::test]
    async fn h3_drain_cancels_admitted_requests_at_the_deadline() {
        let (request_cancel, cancelled) = watch::channel(false);
        let mut requests = tokio::task::JoinSet::new();
        requests.spawn(std::future::pending::<()>());

        join_h3_requests(
            "test HTTP/3",
            &mut requests,
            &request_cancel,
            Instant::now() + Duration::from_millis(10),
        )
        .await;

        assert!(*cancelled.borrow(), "timed-out request was not cancelled");
    }

    #[tokio::test]
    async fn h3_accept_gate_close_is_acknowledged_for_drain() {
        let gate = pingora::apps::AcceptGate::closed();
        gate.enable();
        let mut participant = gate.register();
        let close = gate.close();
        let state = participant.changed().await.expect("accept gate change");

        assert!(!state.accepting);
        participant.acknowledge(state.epoch);
        assert!(close.wait(Duration::from_secs(1)));
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

    #[test]
    fn sanitizes_h3_response_framing_and_hop_by_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("x-hop, keep-alive"));
        headers.insert(
            HeaderName::from_static("x-hop"),
            HeaderValue::from_static("removed"),
        );
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("999"));
        headers.insert(TE, HeaderValue::from_static("trailers"));

        sanitize_h3_response_headers(&mut headers, StatusCode::OK, 3, false)
            .expect("safe H3 response headers");

        assert_eq!(headers[CONTENT_LENGTH], "3");
        assert!(!headers.contains_key(CONNECTION));
        assert!(!headers.contains_key("x-hop"));
        assert!(!headers.contains_key(TE));

        let mut head_headers = HeaderMap::new();
        head_headers.insert(CONTENT_LENGTH, HeaderValue::from_static("9"));
        sanitize_h3_response_headers(&mut head_headers, StatusCode::OK, 0, true)
            .expect("safe H3 HEAD response headers");
        assert_eq!(head_headers[CONTENT_LENGTH], "9");
    }

    #[test]
    fn rejects_unrepresentable_h3_response_and_trailer_shapes() {
        let mut headers = HeaderMap::new();
        assert!(
            sanitize_h3_response_headers(&mut headers, StatusCode::SWITCHING_PROTOCOLS, 0, false,)
                .is_err()
        );

        let mut headers = HeaderMap::new();
        assert!(
            sanitize_h3_response_headers(&mut headers, StatusCode::NO_CONTENT, 1, false,).is_err()
        );

        let mut headers = HeaderMap::new();
        assert!(
            sanitize_h3_response_headers(
                &mut headers,
                StatusCode::OK,
                H3_MAX_RESPONSE_BODY_BYTES + 1,
                false,
            )
            .is_err()
        );

        let mut trailers = HeaderMap::new();
        trailers.insert(CONTENT_LENGTH, HeaderValue::from_static("1"));
        sanitize_h3_trailers(&mut trailers).expect("safe H3 trailers");
        assert!(!trailers.contains_key(CONTENT_LENGTH));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one request-value matrix keeps H3 policy behavior comparable"
    )]
    fn h3_request_values_join_fields_and_honor_source_exceptions() {
        let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
            request_headers: vec![
                HttpRequestHeaderMutation::Set {
                    name: "x-literal".into(),
                    value: HttpRequestHeaderValue::Literal {
                        value: "literal".into(),
                    },
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-authority".into(),
                    value: HttpRequestHeaderValue::IncomingAuthority,
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-normalized".into(),
                    value: HttpRequestHeaderValue::NormalizedHost,
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-nginx".into(),
                    value: HttpRequestHeaderValue::NginxHost {
                        fallback: "fallback.test".into(),
                    },
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-client".into(),
                    value: HttpRequestHeaderValue::ClientIp,
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-forwarded-for".into(),
                    value: HttpRequestHeaderValue::AppendedXForwardedFor {
                        max_bytes: 128,
                        except_source_cidrs: Vec::new(),
                    },
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-scheme".into(),
                    value: HttpRequestHeaderValue::DownstreamScheme,
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-joined".into(),
                    value: HttpRequestHeaderValue::IncomingHeader {
                        name: "x-source".into(),
                        max_bytes: 32,
                    },
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-selected".into(),
                    value: HttpRequestHeaderValue::SelectedUpstreamHost,
                },
                HttpRequestHeaderMutation::Remove {
                    name: "x-remove".into(),
                },
            ],
            ..HttpProxyPolicy::default()
        });
        let authority = "Client.Example.:8080"
            .parse()
            .expect("mixed-case authority");
        let mut incoming = Request::builder()
            .uri("https://Client.Example.:8080/path?q=1")
            .header("x-forwarded-for", "trusted")
            .header("x-source", "one")
            .header("x-remove", "stale")
            .body(())
            .unwrap();
        incoming
            .headers_mut()
            .append("x-source", HeaderValue::from_static("two"));
        let mut headers = incoming.headers().clone();
        let decisions = h3_request_header_decisions(
            &incoming,
            &authority,
            &policy,
            Some("127.0.0.1:1234".parse().unwrap()),
        )
        .expect("H3 request decisions");
        apply_h3_header_mutations(
            &mut headers,
            &decisions,
            &HeaderValue::from_static("selected.example:9443"),
        )
        .expect("H3 request values");
        for (name, expected) in [
            ("x-literal", "literal"),
            ("x-authority", "Client.Example.:8080"),
            ("x-normalized", "client.example."),
            ("x-nginx", "client.example"),
            ("x-client", "127.0.0.1"),
            ("x-forwarded-for", "trusted, 127.0.0.1"),
            ("x-scheme", "https"),
            ("x-joined", "one, two"),
            ("x-selected", "selected.example:9443"),
        ] {
            assert_eq!(
                headers.get(name),
                Some(&HeaderValue::from_static(expected)),
                "{name}"
            );
        }
        assert!(!headers.contains_key("x-remove"));

        let excepted = ProxyPolicyPlan::compile(&HttpProxyPolicy {
            request_headers: vec![HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: vec!["127.0.0.0/8".into()],
                },
            }],
            ..HttpProxyPolicy::default()
        });
        let mut excepted_headers = incoming.headers().clone();
        let decisions = h3_request_header_decisions(
            &incoming,
            &authority,
            &excepted,
            Some("127.0.0.1:1234".parse().unwrap()),
        )
        .expect("excepted H3 request decisions");
        apply_h3_header_mutations(
            &mut excepted_headers,
            &decisions,
            &HeaderValue::from_static("selected.example:9443"),
        )
        .expect("excepted H3 request values");
        assert_eq!(excepted_headers["x-forwarded-for"], "trusted");
    }

    #[test]
    fn h3_join_bounds_include_inserted_separators() {
        let authority = "example.test".parse().unwrap();
        let mut incoming = Request::builder()
            .uri("https://example.test/")
            .header("x-source", "abc")
            .body(())
            .unwrap();
        incoming
            .headers_mut()
            .append("x-source", HeaderValue::from_static("de"));
        let policy = |max_bytes| {
            ProxyPolicyPlan::compile(&HttpProxyPolicy {
                request_headers: vec![HttpRequestHeaderMutation::Set {
                    name: "x-joined".into(),
                    value: HttpRequestHeaderValue::IncomingHeader {
                        name: "x-source".into(),
                        max_bytes,
                    },
                }],
                ..HttpProxyPolicy::default()
            })
        };

        let exact = policy(7);
        let decisions = h3_request_header_decisions(&incoming, &authority, &exact, None).unwrap();
        let mut headers = incoming.headers().clone();
        apply_h3_header_mutations(
            &mut headers,
            &decisions,
            &HeaderValue::from_static("selected.test"),
        )
        .unwrap();
        assert_eq!(headers["x-joined"], "abc, de");

        assert_eq!(
            h3_request_header_decisions(&incoming, &authority, &policy(6), None),
            Err(RequestPolicyError::SourceTooLarge)
        );
    }

    #[test]
    fn h3_missing_peer_xff_is_noop_without_an_existing_value() {
        let authority = "example.test".parse().unwrap();
        let selected = HeaderValue::from_static("selected.test");
        let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
            request_headers: vec![HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            }],
            ..HttpProxyPolicy::default()
        });
        for (existing, expected) in [(None, None), (Some("trusted"), Some("trusted"))] {
            let mut incoming = Request::builder()
                .uri("https://example.test/")
                .body(())
                .unwrap();
            if let Some(existing) = existing {
                incoming
                    .headers_mut()
                    .insert("x-forwarded-for", HeaderValue::from_static(existing));
            }
            let mut headers = incoming.headers().clone();
            let decisions =
                h3_request_header_decisions(&incoming, &authority, &policy, None).unwrap();
            apply_h3_header_mutations(&mut headers, &decisions, &selected).unwrap();
            assert_eq!(
                headers
                    .get("x-forwarded-for")
                    .and_then(|value| value.to_str().ok()),
                expected
            );
        }
    }

    #[test]
    fn h3_redirect_templates_characterize_context_and_bounds() {
        let authority = "Client.Example.:8443".parse().unwrap();
        let uri = "/path/to?q=1".parse().unwrap();
        let template = |value: &str| HttpRedirectLocation::RequestTemplate {
            value: value.into(),
            nginx_host_fallback: Some("fallback.test".into()),
        };
        for (location, expected) in [
            (
                template("$scheme://$host$request_uri"),
                Some("https://client.example/path/to?q=1"),
            ),
            (template("$unknown"), None),
        ] {
            let location = RedirectLocationPlan::compile(&location);
            let actual = location
                .as_ref()
                .and_then(|location| h3_redirect_location(location, &authority, &uri));
            assert_eq!(
                actual.as_ref().and_then(|value| value.to_str().ok()),
                expected
            );
        }
        for (length, accepted) in [(8192, true), (8193, false)] {
            let location = HttpRedirectLocation::Literal {
                value: "x".repeat(length),
            };
            let location = RedirectLocationPlan::compile(&location);
            assert_eq!(
                location
                    .as_ref()
                    .and_then(|location| h3_redirect_location(location, &authority, &uri))
                    .is_some(),
                accepted,
                "literal length {length}"
            );
        }
        let boundary_uri = format!("/{}", "x".repeat(8191)).parse().unwrap();
        assert!(
            RedirectLocationPlan::compile(&template("$request_uri"))
                .and_then(|location| h3_redirect_location(&location, &authority, &boundary_uri))
                .is_some()
        );
        let oversized_uri = format!("/{}", "x".repeat(8192)).parse().unwrap();
        assert!(
            RedirectLocationPlan::compile(&template("$request_uri"))
                .and_then(|location| h3_redirect_location(&location, &authority, &oversized_uri))
                .is_none()
        );
    }
}
