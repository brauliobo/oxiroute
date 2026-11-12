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
    threads: Vec<JoinHandle<()>>,
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
                threads: vec![thread],
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
    /// service app. Proxy routes to an exact H3 pool use the native H3 upstream transport; routes to
    /// every other pool use the classic HTTP/1.1 and HTTP/2 origin connections, with the answer buffered
    /// unless it is long or of unknown length.
    ///
    /// The listener runs `workers` QUIC endpoints when its socket is shared (see
    /// [`ListenerReservation::quic_endpoints`]), each served by a thread of its own: the kernel gives
    /// every client address to one of them, so connections spread over the cores.
    ///
    /// # Errors
    ///
    /// Returns an error unless the QUIC endpoints, TLS configuration, and H3 accept loops all reach
    /// their startup boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn start_reverse(
        listener_name: &str,
        reservation: &ListenerReservation,
        service: &Arc<HttpServicePlan>,
        tls: &Arc<TlsProfilePlan>,
        generation: &Arc<RuntimeGeneration>,
        metrics: &ListenerMetrics,
        downstream_timeouts: DownstreamTimeoutPolicy,
        shutdown: &watch::Receiver<bool>,
        workers: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let server_config = Arc::new(server_config(
            tls,
            downstream_timeouts
                .keepalive_timeout_ms
                .map(Duration::from_millis),
        )?);
        let endpoints = reservation.quic_endpoints(&server_config, workers)?;
        metrics.mark_listening();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(endpoints.len());
        let mut threads = Vec::with_capacity(endpoints.len());
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            let worker = Worker {
                endpoint,
                server_config: Arc::clone(&server_config),
                service: Arc::clone(service),
                generation: Arc::clone(generation),
                metrics: metrics.clone(),
                downstream_timeouts,
                shutdown: shutdown.clone(),
                // One worker owns the listener's state in the metrics, as the only endpoint did.
                reports_state: index == 0,
            };
            let (name, ready) = (listener_name.to_owned(), ready_tx.clone());
            let generation = Arc::clone(generation);
            let thread = thread::Builder::new()
                .name(format!("oxiroute-h3-reverse-{listener_name}"))
                .spawn(move || {
                    if let Err(error) = worker.run(&name, &ready) {
                        generation.mark_runtime_failed();
                        error!("reverse HTTP/3 listener `{name}` failed: {error}");
                    }
                })?;
            threads.push(thread);
        }
        drop(ready_tx);
        for _ in 0..threads.len() {
            ready_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| {
                    let timed_out = error == std::sync::mpsc::RecvTimeoutError::Timeout;
                    io::Error::new(
                        if timed_out {
                            io::ErrorKind::TimedOut
                        } else {
                            io::ErrorKind::BrokenPipe
                        },
                        format!(
                            "reverse HTTP/3 listener `{listener_name}` did not become ready: {error}"
                        ),
                    )
                })?
                .map_err(io::Error::other)?;
        }
        Ok(Self { threads })
    }

    /// Waits for the listener and all active H3 connections to stop.
    ///
    /// # Errors
    ///
    /// Returns an error if a listener thread panicked.
    pub fn join(self) -> io::Result<()> {
        let mut result = Ok(());
        for thread in self.threads {
            if thread.join().is_err() {
                result = Err(io::Error::other("HTTP/3 listener thread panicked"));
            }
        }
        result
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
        let endpoint = reservation
            .quic_endpoints(&server_config, 1)?
            .swap_remove(0);
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

/// One QUIC endpoint of a reverse HTTP/3 listener and what its thread serves it with.
struct Worker {
    endpoint: Endpoint,
    server_config: Arc<ServerConfig>,
    service: Arc<HttpServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    downstream_timeouts: DownstreamTimeoutPolicy,
    shutdown: watch::Receiver<bool>,
    /// This worker marks the listener stopped or failed when it ends.
    reports_state: bool,
}

impl Worker {
    fn run(
        self,
        listener_name: &str,
        ready: &std::sync::mpsc::SyncSender<Result<(), String>>,
    ) -> Result<(), Box<dyn Error>> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        runtime.block_on(async move {
            ready
                .send(Ok(()))
                .map_err(|_| io::Error::other("reverse HTTP/3 startup receiver was dropped"))?;
            let listener_metrics = self.metrics.clone();
            let listener = ListenerRuntime::new(self.metrics.clone());
            let accept_gate = self.generation.accept_gate().register();
            let orderly = serve_reverse_endpoint(
                listener_name,
                self.endpoint,
                self.server_config,
                self.service,
                self.generation,
                listener,
                self.metrics,
                self.downstream_timeouts,
                Arc::new(OriginClient::new()),
                self.shutdown,
                accept_gate,
            )
            .await;
            if orderly {
                if self.reports_state {
                    listener_metrics.mark_stopped();
                }
            } else {
                if self.reports_state {
                    listener_metrics.mark_failed();
                }
                return Err(
                    io::Error::other("reverse HTTP/3 endpoint terminated unexpectedly").into(),
                );
            }
            Ok::<(), Box<dyn Error>>(())
        })
    }
}

/// The ring provider with the profile's key-exchange groups in the profile's order; the provider has no hybrid group.
pub(super) fn h3_crypto_provider(
    groups: Option<&[TlsKeyExchangeGroup]>,
) -> io::Result<Arc<rustls::crypto::CryptoProvider>> {
    crate::tls::provider::with_key_exchange_groups(crate::tls::provider::ring_provider(), groups)
        .map_err(io::Error::other)
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
    let mut crypto = match tls.rustls_client_cert_verifier() {
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
    origin: Arc<OriginClient>,
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
                let origin = Arc::clone(&origin);
                connections.spawn(async move {
                    run_reverse_connection(
                        incoming,
                        server_config,
                        service,
                        generation,
                        listener,
                        metrics,
                        downstream_timeouts,
                        origin,
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
        let generation = self.profile.selected_generation(client_hello.server_name());
        match generation.certified_key() {
            Ok(key) => Some(Arc::clone(key)),
            Err(error) => {
                warn!("QUIC certificate selection failed: {error}");
                None
            }
        }
    }
}
