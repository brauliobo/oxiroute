#[allow(clippy::wildcard_imports)]
use super::*;

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
/// Bound for H3 work when the whole process is stopping.
pub(super) const H3_GRACEFUL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const H3_CLOSE_GRACE: Duration = Duration::from_secs(1);
pub(super) const H3_CLOSE_CODE: VarInt = VarInt::from_u32(0x100);

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
        let server_config = Arc::new(server_config(&tls, None)?);
        let endpoint = reservation.quic_endpoint(ServerConfig::clone(&server_config))?;
        metrics.mark_listening();
        ready
            .send(Ok(()))
            .map_err(|_| io::Error::other("HTTP/3 startup receiver was dropped"))?;
        let listener_metrics = metrics.clone();
        let listener = ListenerRuntime::new(metrics.clone());
        let accept_gate = generation.accept_gate().register();
        let orderly = serve_endpoint(
            listener_name,
            endpoint,
            server_config,
            service,
            generation,
            listener,
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
        let server_config = Arc::new(server_config(
            &tls,
            downstream_timeouts
                .keepalive_timeout_ms
                .map(Duration::from_millis),
        )?);
        let endpoint = reservation.quic_endpoint(ServerConfig::clone(&server_config))?;
        metrics.mark_listening();
        ready
            .send(Ok(()))
            .map_err(|_| io::Error::other("reverse HTTP/3 startup receiver was dropped"))?;
        let listener_metrics = metrics.clone();
        let listener = ListenerRuntime::new(metrics.clone());
        let accept_gate = generation.accept_gate().register();
        let orderly = serve_reverse_endpoint(
            listener_name,
            endpoint,
            server_config,
            service,
            generation,
            listener,
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

/// The ring provider with the profile's key-exchange groups in the profile's order; the provider has no hybrid group.
pub(super) fn h3_crypto_provider(
    groups: Option<&[TlsKeyExchangeGroup]>,
) -> io::Result<Arc<rustls::crypto::CryptoProvider>> {
    let mut provider = rustls::crypto::ring::default_provider();
    if let Some(groups) = groups {
        let available = std::mem::take(&mut provider.kx_groups);
        provider.kx_groups = groups
            .iter()
            .filter_map(|&group| {
                available
                    .iter()
                    .copied()
                    .find(|kx| kx.name() == named_group(group))
            })
            .collect();
        if provider.kx_groups.is_empty() {
            return Err(io::Error::other(
                "no key_exchange_groups entry is available to HTTP/3",
            ));
        }
    }
    Ok(Arc::new(provider))
}

const fn named_group(group: TlsKeyExchangeGroup) -> rustls::NamedGroup {
    match group {
        TlsKeyExchangeGroup::X25519 => rustls::NamedGroup::X25519,
        TlsKeyExchangeGroup::Secp256r1 => rustls::NamedGroup::secp256r1,
        TlsKeyExchangeGroup::Secp384r1 => rustls::NamedGroup::secp384r1,
        TlsKeyExchangeGroup::X25519Mlkem768 => rustls::NamedGroup::X25519MLKEM768,
    }
}

fn server_config(
    tls: &Arc<TlsProfilePlan>,
    idle_timeout: Option<Duration>,
) -> io::Result<ServerConfig> {
    let resolver = Arc::new(QuicCertificateResolver {
        profile: Arc::clone(tls),
    });
    let crypto_builder = rustls::ServerConfig::builder_with_provider(h3_crypto_provider(
        tls.policy().key_exchange_groups.as_deref(),
    )?)
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(io::Error::other)?;
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

/// How long H3 requests may keep running once their endpoint stops accepting: the generation's
/// drain deadline, or the short process-stop bound when the whole process is stopping.
pub(super) fn h3_drain_window(
    generation: &RuntimeGeneration,
    shutdown: &watch::Receiver<bool>,
) -> Duration {
    if *shutdown.borrow() {
        H3_GRACEFUL_DRAIN_TIMEOUT
    } else {
        generation.retirement_deadline()
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve_endpoint(
    listener_name: &str,
    endpoint: Endpoint,
    server_config: Arc<ServerConfig>,
    service: Arc<ForwardHttp1ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    listener: ListenerRuntime,
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
            biased;
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
            incoming = endpoint.accept(), if gate_state.accepting => {
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
                let listener = listener.clone();
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                let drain = drain_rx.clone();
                let server_config = Arc::clone(&server_config);
                connections.spawn(async move {
                    run_connection(
                        incoming,
                        server_config,
                        service,
                        generation,
                        listener,
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
    let window = h3_drain_window(&generation, &shutdown);
    finish_h3_connections(listener_name, &mut connections, window).await;
    orderly
}

#[allow(clippy::too_many_arguments)]
async fn serve_reverse_endpoint(
    listener_name: &str,
    endpoint: Endpoint,
    server_config: Arc<ServerConfig>,
    service: Arc<HttpServicePlan>,
    generation: Arc<RuntimeGeneration>,
    listener: ListenerRuntime,
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
            biased;
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
            incoming = endpoint.accept(), if gate_state.accepting => {
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
                let listener = listener.clone();
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                let drain = drain_rx.clone();
                let server_config = Arc::clone(&server_config);
                connections.spawn(async move {
                    run_reverse_connection(
                        incoming,
                        server_config,
                        service,
                        generation,
                        listener,
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
    let window = h3_drain_window(&generation, &shutdown);
    finish_h3_connections(listener_name, &mut connections, window).await;
    orderly
}

async fn finish_h3_connections(
    listener_name: &str,
    connections: &mut tokio::task::JoinSet<()>,
    window: Duration,
) {
    let deadline = Instant::now() + window;
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
