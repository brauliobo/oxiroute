use std::{
    error::Error,
    io,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use futures_util::FutureExt as _;
use log::{error, info, warn};
use oxiroute_config::ListenerBind;
use oxiroute_rtmp::{MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, RtmpRegistry, RtmpServiceRuntime};
use oxiroute_server::{
    CertbotWatcherConfig, CertbotWatcherSupervisor, ConfigWatcher, ConfigWatcherOptions,
    GenerationManager, HaproxyStatsApi, HttpDownstreamPolicyApp, HttpListenerApp, HttpReverseProxy,
    ListenerMetrics, ListenerReservation, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RtmpManagementApi,
    RuntimeGeneration, RuntimeMetrics, RuntimeReferenceKind, ServiceKind, TcpRelayCore,
    TlsProfilePlan, TopologySnapshot,
    cli::{Cli, Command, ConfigCommand, execute_offline},
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigRevision},
};
use pingora::{
    apps::http_app::HttpServer,
    apps::{AcceptGate, ConnectionAdmission, ServerApp},
    protocols::Stream,
    proxy::http_proxy,
    server::{
        RunArgs, Server, ShutdownSignal, ShutdownSignalWatch, ShutdownWatch,
        configuration::ServerConf,
    },
    services::{
        Service as PingoraService, ServiceReadyNotifier, ServiceWithDependents,
        background::background_service, listening::Service,
    },
};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    iterator::Signals,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{MissedTickBehavior, interval, timeout},
};

const RTMP_READ_BUFFER_SIZE: usize = 16 * 1024;
const RTMP_PLAYBACK_DRAIN_INTERVAL: Duration = Duration::from_millis(10);
const RTMP_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

fn add_plain_listener<A>(
    service: &mut Service<A>,
    listener_name: &str,
    bind: &ListenerBind,
) -> io::Result<()> {
    match bind {
        ListenerBind::Socket { address } => {
            service.add_tcp(&address.to_string());
            Ok(())
        }
        ListenerBind::Udp { address } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("listener `{listener_name}` cannot register UDP socket `{address}` yet"),
        )),
        ListenerBind::Unix { path, .. } => {
            let path = path.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "listener `{listener_name}` Unix socket path is not valid UTF-8 and cannot be registered"
                    ),
                )
            })?;
            #[cfg(unix)]
            {
                service.add_uds(path, None);
                Ok(())
            }
            #[cfg(not(unix))]
            {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "listener `{listener_name}` cannot register Unix socket `{path}` on this platform"
                    ),
                ))
            }
        }
    }
}

struct RuntimeListenerService<S> {
    generation: Option<Arc<RuntimeGeneration>>,
    inner: S,
    metrics: Option<ListenerMetrics>,
    reservation: ListenerReservation,
}

impl<S> RuntimeListenerService<S> {
    fn new(inner: S, reservation: ListenerReservation, metrics: Option<ListenerMetrics>) -> Self {
        Self {
            generation: None,
            inner,
            metrics,
            reservation,
        }
    }

    fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }
}

#[async_trait]
impl<S> ServiceWithDependents for RuntimeListenerService<S>
where
    S: PingoraService + Send + Sync,
{
    async fn start_service(
        &mut self,
        #[cfg(unix)] _inherited_fds: Option<pingora::server::ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) {
        let shutdown_status = shutdown.clone();
        #[cfg(unix)]
        let fds = self
            .reservation
            .duplicate_fds()
            .expect("prepared listener descriptor can be duplicated");
        #[cfg(unix)]
        let fds = Some(Arc::new(tokio::sync::Mutex::new(fds)));
        let service_name = self.inner.name().to_owned();
        let result = AssertUnwindSafe(PingoraService::start_service(
            &mut self.inner,
            #[cfg(unix)]
            fds,
            shutdown,
            listeners_per_fd,
        ))
        .catch_unwind();
        futures_util::pin_mut!(result);
        let result = tokio::select! {
            result = &mut result => result,
            () = tokio::time::sleep(Duration::from_millis(10)) => {
                if let Some(metrics) = &self.metrics {
                    metrics.mark_listening();
                }
                ready_notifier.notify_ready();
                result.await
            }
        };
        let unexpected = !*shutdown_status.borrow();
        if let Some(metrics) = &self.metrics {
            if result.is_ok() && !unexpected {
                metrics.mark_stopped();
            } else {
                metrics.mark_failed();
            }
        }
        if result.is_err() || unexpected {
            if let Some(generation) = &self.generation {
                generation.mark_runtime_failed();
            }
            error!("listener service `{service_name}` terminated unexpectedly");
        }
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn threads(&self) -> Option<usize> {
        self.inner.threads()
    }
}

struct GenerationRuntimeMarker {
    generation: Arc<RuntimeGeneration>,
}

#[async_trait]
impl ServiceWithDependents for GenerationRuntimeMarker {
    async fn start_service(
        &mut self,
        #[cfg(unix)] _fds: Option<pingora::server::ListenFds>,
        mut shutdown: ShutdownWatch,
        _listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) {
        self.generation.mark_runtime_started();
        ready_notifier.notify_ready();
        while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
    }

    fn name(&self) -> &'static str {
        "OxiRoute generation runtime marker"
    }
}

fn add_http_listener<A>(
    service: &mut Service<A>,
    listener_name: &str,
    bind: &ListenerBind,
    tls: Option<&TlsProfilePlan>,
) -> Result<(), Box<dyn Error>> {
    match (bind, tls) {
        (ListenerBind::Socket { address }, Some(tls)) => {
            service.add_tls_with_settings(&address.to_string(), None, tls.tls_settings()?);
            Ok(())
        }
        (ListenerBind::Udp { .. }, Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("listener `{listener_name}` cannot register stream TLS on a UDP socket"),
        )
        .into()),
        (ListenerBind::Unix { .. }, Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("listener `{listener_name}` cannot register TLS on a Unix socket"),
        )
        .into()),
        (_, None) => {
            add_plain_listener(service, listener_name, bind)?;
            Ok(())
        }
    }
}

fn admit_connection(metrics: &ListenerMetrics) -> Option<ConnectionAdmission> {
    match metrics.begin_connection() {
        Ok(connection) => Some(Box::new(connection)),
        Err(error) => {
            warn!(
                "rejected connection on listener `{}`: {error}",
                metrics.name()
            );
            None
        }
    }
}

struct ProcessAdmissionApp<A> {
    generation: Arc<RuntimeGeneration>,
    inner: Arc<A>,
    metrics: RuntimeMetrics,
}

impl<A> ProcessAdmissionApp<A> {
    fn new(inner: A, metrics: RuntimeMetrics, generation: Arc<RuntimeGeneration>) -> Self {
        Self {
            generation,
            inner: Arc::new(inner),
            metrics,
        }
    }
}

#[async_trait]
impl<A> ServerApp for ProcessAdmissionApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.generation.accepting() && self.inner.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let generation_admission = self.generation.begin_admission()?;
        let generation_reference = self
            .generation
            .begin_reference(RuntimeReferenceKind::Http1)?;
        let process = match self.metrics.begin_control_connection() {
            Ok(process) => process,
            Err(error) => {
                warn!("rejected management connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((
            generation_admission,
            generation_reference,
            process,
            inner,
        )))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let generation_reference = self
            .generation
            .begin_owned_reference(RuntimeReferenceKind::Http1);
        let process = match self.metrics.begin_control_connection() {
            Ok(process) => process,
            Err(error) => {
                warn!("rejected management connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((generation_reference, process, inner)))
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        self.inner.process_new(downstream, shutdown).await
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

struct TcpRelay {
    generation: Option<Arc<RuntimeGeneration>>,
    service: Arc<oxiroute_server::L4ServicePlan>,
    metrics: ListenerMetrics,
}

impl TcpRelay {
    fn new(service: Arc<oxiroute_server::L4ServicePlan>, metrics: ListenerMetrics) -> Self {
        Self {
            generation: None,
            service,
            metrics,
        }
    }

    fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }
}

#[async_trait]
impl ServerApp for TcpRelay {
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.generation
            .as_ref()
            .map(|generation| generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.metrics.accepting()
            && self
                .generation
                .as_ref()
                .is_none_or(|generation| generation.accepting())
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let generation = if let Some(generation) = &self.generation {
            Some(generation.begin_reference(RuntimeReferenceKind::Tcp)?)
        } else {
            None
        };
        let connection = admit_connection(&self.metrics)?;
        Some(Box::new((generation, connection)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let generation = self
            .generation
            .as_ref()
            .map(|generation| generation.begin_owned_reference(RuntimeReferenceKind::Tcp));
        let connection = admit_connection(&self.metrics)?;
        Some(Box::new((generation, connection)))
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let connection = self.metrics.traffic_accounting();
        let Some(upstream) = self.service.select_wait().await else {
            warn!("TCP pool has no healthy upstream");
            return None;
        };
        let relay = TcpRelayCore::new(upstream, self.service.policy());
        if let Err(error) = relay.relay(downstream, &connection, shutdown.clone()).await {
            warn!("TCP relay failed: {error}");
        }

        None
    }
}

struct RtmpIngest {
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    runtime: RtmpServiceRuntime,
}

impl RtmpIngest {
    fn new(
        runtime: RtmpServiceRuntime,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            generation,
            metrics,
            runtime,
        }
    }
}

#[async_trait]
impl ServerApp for RtmpIngest {
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.metrics.accepting() && self.generation.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let generation = self
            .generation
            .begin_reference(RuntimeReferenceKind::Rtmp)?;
        let connection = admit_connection(&self.metrics)?;
        Some(Box::new((generation, connection)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let generation = self
            .generation
            .begin_owned_reference(RuntimeReferenceKind::Rtmp);
        let connection = admit_connection(&self.metrics)?;
        Some(Box::new((generation, connection)))
    }

    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let Some(mut at_unix_ms) = unix_time_ms() else {
            warn!("cannot start RTMP session because the system clock is invalid");
            return None;
        };
        let mut session = self.runtime.session();
        let mut buffer = [0; RTMP_READ_BUFFER_SIZE];
        let mut shutdown = shutdown.clone();
        let mut playback_drain = interval(RTMP_PLAYBACK_DRAIN_INTERVAL);
        playback_drain.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let outbound = tokio::select! {
                _ = shutdown.changed() => break,
                _ = playback_drain.tick(), if session.is_playback_active() => {
                    match session.drain_playback(MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN) {
                        Ok(outbound) => outbound,
                        Err(error) => {
                            warn!("RTMP playback serialization failed: {error}");
                            break;
                        }
                    }
                }
                read = downstream.read(&mut buffer) => {
                    let bytes_read = match read {
                        Ok(0) => break,
                        Ok(bytes_read) => bytes_read,
                        Err(error) => {
                            warn!("RTMP transport read failed: {error}");
                            break;
                        }
                    };
                    let Ok(bytes_received) = u64::try_from(bytes_read) else {
                        warn!("RTMP read size exceeds the supported metrics range");
                        break;
                    };
                    if let Err(error) = self.metrics.record_bytes_received(bytes_received) {
                        warn!("could not account for RTMP ingress: {error}");
                        break;
                    }
                    let Some(now_unix_ms) = unix_time_ms() else {
                        warn!("closing RTMP session because the system clock is invalid");
                        break;
                    };
                    at_unix_ms = now_unix_ms;

                    match session.receive(&buffer[..bytes_read], at_unix_ms) {
                        Ok(outbound) => outbound,
                        Err(error) => {
                            warn!("RTMP session failed: {error}");
                            break;
                        }
                    }
                }
            };
            if outbound.is_empty() {
                continue;
            }
            if let Err(error) = write_rtmp_packets(&mut downstream, &outbound, &self.metrics).await
            {
                warn!("RTMP transport write failed: {error}");
                break;
            }
        }

        if let Err(error) = session.close(at_unix_ms) {
            warn!("could not detach RTMP media role: {error}");
        }
        None
    }
}

async fn write_rtmp_packets(
    downstream: &mut Stream,
    packets: &[Vec<u8>],
    metrics: &ListenerMetrics,
) -> io::Result<()> {
    timeout(RTMP_WRITE_TIMEOUT, async {
        for packet in packets {
            let mut remaining = packet.as_slice();
            while !remaining.is_empty() {
                let bytes_written = downstream.write(remaining).await?;
                if bytes_written == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "RTMP transport accepted no response bytes",
                    ));
                }
                let bytes_sent = u64::try_from(bytes_written)
                    .map_err(|_| io::Error::other("RTMP write size exceeds u64"))?;
                metrics
                    .record_bytes_sent(bytes_sent)
                    .map_err(io::Error::other)?;
                remaining = &remaining[bytes_written..];
            }
        }
        downstream.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "RTMP write deadline exceeded"))?
}

fn build_management_api(
    registry: Arc<RtmpRegistry>,
    metrics: RuntimeMetrics,
    topology: Arc<TopologySnapshot>,
    coordinator: CanonicalConfigCoordinator,
    active_revision: ConfigRevision,
    token_file: &Path,
    ui_dir: Option<&Path>,
) -> io::Result<RtmpManagementApi> {
    let api = if let Some(ui_dir) = ui_dir {
        RtmpManagementApi::with_ui_dir(registry, metrics, topology, ui_dir)
    } else {
        Ok(RtmpManagementApi::new(registry, metrics, topology))
    }?;
    api.with_config_coordinator_from_token_file(coordinator, active_revision, token_file)
}

fn main() -> ExitCode {
    env_logger::init();

    let cli = Cli::parse_process();
    let result = match cli.command() {
        Command::Serve { config } => run(config),
        Command::Import { .. }
        | Command::Version
        | Command::Config {
            command: ConfigCommand::Check { .. } | ConfigCommand::Compose { .. },
        } => execute_offline(cli.command()).map(|output| {
            if let Some(output) = output {
                print!("{output}");
            }
        }),
        _ => return cli.execute_management(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OxiRoute startup failed: {error}");
            ExitCode::FAILURE
        }
    }
}

struct ChannelShutdownSignal {
    shutdown: tokio::sync::watch::Receiver<bool>,
}

#[async_trait]
impl ShutdownSignalWatch for ChannelShutdownSignal {
    async fn recv(&self) -> ShutdownSignal {
        let mut shutdown = self.shutdown.clone();
        if !*shutdown.borrow() {
            let _ = shutdown.changed().await;
        }
        ShutdownSignal::GracefulTerminate
    }
}

struct GenerationProcess {
    generation: Arc<RuntimeGeneration>,
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: JoinHandle<()>,
}

impl GenerationProcess {
    fn start(
        generation: Arc<RuntimeGeneration>,
        coordinator: CanonicalConfigCoordinator,
        manager: GenerationManager,
        process_shutdown: &Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn Error>> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (setup_tx, setup_rx) = mpsc::sync_channel(1);
        let thread_generation = Arc::clone(&generation);
        let thread_process_shutdown = Arc::clone(process_shutdown);
        let thread = thread::Builder::new()
            .name(format!(
                "oxiroute-generation-{}",
                &generation.revision().disk.as_str()[..12]
            ))
            .spawn(move || {
                if let Err(error) = serve_generation(
                    &thread_generation,
                    &coordinator,
                    &manager,
                    thread_process_shutdown,
                    shutdown_rx,
                    &setup_tx,
                ) {
                    let _ = setup_tx.try_send(Err(error.to_string()));
                    error!("generation runtime failed: {error}");
                }
            })?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if process_shutdown.load(Ordering::Acquire) {
                let _ = shutdown_tx.send(true);
                drop(thread);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "generation startup was cancelled",
                )
                .into());
            }
            match setup_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(())) => {
                    return Ok(Self {
                        generation,
                        shutdown: shutdown_tx,
                        thread,
                    });
                }
                Ok(Err(error)) => {
                    let _ = shutdown_tx.send(true);
                    drop(thread);
                    return Err(io::Error::other(error).into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    drop(thread);
                    return Err(
                        io::Error::other("generation setup terminated without readiness").into(),
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) if std::time::Instant::now() >= deadline => {
                    let _ = shutdown_tx.send(true);
                    drop(thread);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "generation setup did not complete before its deadline",
                    )
                    .into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn request_shutdown(&self) {
        self.generation.stop_accepting();
        let _ = self.shutdown.send(true);
    }

    fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    fn join(self) {
        self.request_shutdown();
        let _ = self.thread.join();
    }
}

fn supervise_generations(
    manager: &GenerationManager,
    coordinator: &CanonicalConfigCoordinator,
    mut current: GenerationProcess,
    stop: &AtomicBool,
    process_shutdown: &Arc<AtomicBool>,
) -> bool {
    let mut retired = Vec::<GenerationProcess>::new();
    while !stop.load(Ordering::Acquire) {
        if current.is_finished() || current.generation.runtime_failed() {
            error!("active generation runtime terminated unexpectedly");
            process_shutdown.store(true, Ordering::Release);
            return false;
        }
        retired = retired
            .into_iter()
            .filter_map(|process| {
                if process.generation.drained() {
                    process.request_shutdown();
                }
                if process.is_finished() {
                    process.join();
                    None
                } else {
                    Some(process)
                }
            })
            .collect();
        let Some(candidate) = manager.candidate() else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        let startup = match manager.begin_candidate_start(&candidate) {
            Ok(startup) => startup,
            Err(oxiroute_server::GenerationError::MutationInProgress) => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => continue,
        };

        match GenerationProcess::start(
            Arc::clone(startup.generation()),
            coordinator.clone(),
            manager.clone(),
            process_shutdown,
        )
        .and_then(|process| {
            if let Err(error) = wait_for_generation_ready(candidate.generation(), stop) {
                process.request_shutdown();
                return Err(error);
            }
            Ok(process)
        }) {
            Ok(next) => match startup.activate() {
                Ok(_) => {
                    retired.push(current);
                    current = next;
                }
                Err(error) => {
                    error!("candidate generation publication failed: {error}");
                    next.join();
                }
            },
            Err(error) => {
                error!("candidate generation could not start: {error}");
                manager.quarantine(&candidate, "runtime_start");
            }
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    current.generation.stop_accepting();
    for process in &retired {
        process.generation.stop_accepting();
    }
    while std::time::Instant::now() < deadline
        && (!current.generation.drained()
            || retired.iter().any(|process| !process.generation.drained()))
    {
        thread::sleep(Duration::from_millis(10));
    }
    current.request_shutdown();
    for process in &retired {
        process.request_shutdown();
    }
    current.join();
    for process in retired {
        process.join();
    }
    true
}

fn wait_for_generation_ready(
    generation: &RuntimeGeneration,
    stop: &AtomicBool,
) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "generation startup was cancelled",
            )
            .into());
        }
        if generation.runtime_failed() {
            return Err(io::Error::other("generation runtime failed during startup").into());
        }
        let snapshot = generation.metrics().snapshot()?;
        if generation.runtime_started()
            && snapshot
                .listeners
                .iter()
                .all(|listener| listener.state == oxiroute_server::ListenerRuntimeState::Listening)
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "generation listeners did not become ready",
            )
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[allow(clippy::too_many_lines)]
fn run(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let stop_supervisor = Arc::new(AtomicBool::new(false));
    let mut signals = Signals::new([SIGTERM, SIGINT])?;
    let signal_handle = signals.handle();
    let signal_stop = Arc::clone(&stop_supervisor);
    let signal_thread = thread::Builder::new()
        .name("oxiroute-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some() {
                signal_stop.store(true, Ordering::Release);
            }
        })?;
    let config_coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let config_document = match config_coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(rejection) => {
            let message = rejection.diagnostics.first().map_or_else(
                || "canonical configuration was rejected".to_owned(),
                |diagnostic| {
                    format!(
                        "canonical configuration was rejected ({}): {}",
                        diagnostic.code, diagnostic.message
                    )
                },
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, message).into());
        }
    };
    if stop_supervisor.load(Ordering::Acquire) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "startup was cancelled").into());
    }
    let generation_manager = GenerationManager::new();
    let candidate = generation_manager.prepare(*config_document)?;
    let initial = GenerationProcess::start(
        Arc::clone(candidate.generation()),
        config_coordinator.clone(),
        generation_manager.clone(),
        &stop_supervisor,
    )?;
    if let Err(error) = wait_for_generation_ready(candidate.generation(), &stop_supervisor) {
        initial.join();
        return Err(error);
    }
    generation_manager.activate(&candidate)?;
    let supervisor_stop = Arc::clone(&stop_supervisor);
    let supervisor_manager = generation_manager.clone();
    let supervisor_coordinator = config_coordinator.clone();
    let supervisor_healthy = Arc::new(AtomicBool::new(true));
    let thread_supervisor_healthy = Arc::clone(&supervisor_healthy);
    let supervisor = thread::Builder::new()
        .name("oxiroute-generation-supervisor".into())
        .spawn(move || {
            if !supervise_generations(
                &supervisor_manager,
                &supervisor_coordinator,
                initial,
                &supervisor_stop,
                &supervisor_stop,
            ) {
                thread_supervisor_healthy.store(false, Ordering::Release);
            }
        })?;
    let mut config_watcher = ConfigWatcher::start(
        config_coordinator,
        generation_manager.clone(),
        ConfigWatcherOptions::default(),
    )?;
    while !stop_supervisor.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(20));
    }
    signal_handle.close();
    signal_thread
        .join()
        .map_err(|_| io::Error::other("signal thread terminated unexpectedly"))?;
    config_watcher.shutdown();
    supervisor
        .join()
        .map_err(|_| io::Error::other("generation supervisor terminated unexpectedly"))?;
    if supervisor_healthy.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(io::Error::other("active generation runtime terminated unexpectedly").into())
    }
}

#[allow(clippy::too_many_lines)]
fn serve_generation(
    generation: &Arc<RuntimeGeneration>,
    config_coordinator: &CanonicalConfigCoordinator,
    generation_manager: &GenerationManager,
    process_shutdown: Arc<AtomicBool>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    setup: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), Box<dyn Error>> {
    let active_revision = generation.revision().disk.clone();
    let config = generation.config();
    let management_token_file = if config.management.is_some() {
        Some(PathBuf::from(
            std::env::var_os("OXIROUTE_MANAGEMENT_TOKEN_FILE").ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "authenticated configuration management requires OXIROUTE_MANAGEMENT_TOKEN_FILE",
                )
            })?,
        ))
    } else {
        None
    };

    let server_config = ServerConf {
        max_retries: MAX_HTTP_ATTEMPTS,
        grace_period_seconds: Some(0),
        graceful_shutdown_timeout_seconds: Some(0),
        ..ServerConf::default()
    };
    let mut server = Server::new_with_opt_and_conf(None, server_config);
    server.bootstrap();
    server.add_service(GenerationRuntimeMarker {
        generation: Arc::clone(generation),
    });

    let plan = generation.plan();
    let services = plan.services.clone();
    let health_supervisor = plan.health_supervisor.clone();
    let pools = plan.pools.clone();
    let tls = &plan.tls;
    let topology = Arc::clone(&plan.topology);
    let certbot_reconcilers = tls.certbot_reconcilers().to_vec();
    if let Some(supervisor) = health_supervisor {
        server.add_service(background_service("upstream health", supervisor));
    }
    let runtime_metrics = generation.metrics().clone();
    let mut monitored_services = Vec::with_capacity(services.len());
    for spec in services {
        let metrics = runtime_metrics.register_configured_listener(
            &spec.name,
            spec.kind.protocol(),
            &spec.bind,
            spec.max_connections,
        )?;
        let reservation = generation
            .reservations()
            .get(&spec.name)
            .cloned()
            .expect("candidate reserved every planned listener");
        monitored_services.push((spec, metrics, reservation));
    }
    let services = monitored_services;
    let management_reservation = config
        .management
        .as_ref()
        .map(|_| {
            generation
                .reservations()
                .get("@management")
                .cloned()
                .ok_or_else(|| io::Error::other("management listener was not reserved"))
        })
        .transpose()?;
    let rtmp_registry = Arc::clone(generation.registry());
    if let Some(management) = &config.management {
        let management_api = build_management_api(
            Arc::clone(&rtmp_registry),
            runtime_metrics.clone(),
            Arc::clone(&topology),
            config_coordinator.clone(),
            active_revision.clone(),
            management_token_file
                .as_deref()
                .expect("management token path was required above"),
            management.ui_dir.as_deref(),
        )?
        .with_generation_manager(generation_manager.clone())
        .with_process_shutdown(process_shutdown);
        let app = ProcessAdmissionApp::new(
            HttpListenerApp::new(HttpServer::new_app(management_api), None)
                .with_generation(Arc::clone(generation)),
            runtime_metrics.clone(),
            Arc::clone(generation),
        );
        let mut service = Service::new("OxiRoute management".into(), app);
        service.add_tcp(&management.bind.to_string());
        server.add_service(
            RuntimeListenerService::new(
                service,
                management_reservation.expect("management listener reservation was prepared above"),
                None,
            )
            .with_generation(Arc::clone(generation)),
        );
        info!("configured management API on {}", management.bind);
    }
    if let Some(stats) = &config.stats {
        for (index, bind) in stats.binds.iter().enumerate() {
            let api = HaproxyStatsApi::new(
                runtime_metrics.clone(),
                pools.clone(),
                Arc::clone(&rtmp_registry),
                generation_manager.clone(),
                stats.admin_token_file.as_deref(),
            )?;
            let stats_app = ProcessAdmissionApp::new(
                HttpListenerApp::new(HttpServer::new_app(api), None)
                    .with_generation(Arc::clone(generation)),
                runtime_metrics.clone(),
                Arc::clone(generation),
            );
            let mut service = Service::new(format!("OxiRoute statistics {index}"), stats_app);
            service.add_tcp(&bind.to_string());
            let reservation = generation
                .reservations()
                .get(&format!("@stats-{index}"))
                .cloned()
                .expect("candidate reserved every statistics listener");
            server.add_service(
                RuntimeListenerService::new(service, reservation, None)
                    .with_generation(Arc::clone(generation)),
            );
            info!("configured HAProxy-compatible statistics on {bind}");
        }
    }

    for (spec, metrics, reservation) in services {
        let listener_name = spec.name;
        let listener_bind = spec.bind;
        let service_name = format!("OxiRoute {listener_name}");
        let listener_tls = spec.tls;
        let downstream_timeouts = spec.downstream_timeouts;

        match spec.kind {
            ServiceKind::Http(http_service) => {
                let proxy = http_proxy(
                    &server.configuration,
                    HttpReverseProxy::new(http_service, metrics.clone())
                        .with_generation(Arc::clone(generation)),
                );
                let app = MonitoredHttpApp::new(
                    HttpListenerApp::new(
                        HttpDownstreamPolicyApp::new(proxy, downstream_timeouts),
                        listener_tls.as_deref(),
                    )
                    .with_generation(Arc::clone(generation)),
                    metrics.clone(),
                )
                .with_generation(Arc::clone(generation));
                let mut service = Service::new(service_name, app);
                add_http_listener(
                    &mut service,
                    &listener_name,
                    &listener_bind,
                    listener_tls.as_deref(),
                )?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::Rtmp(rtmp_service) => {
                let runtime = generation
                    .rtmp_runtime(rtmp_service.service_id())
                    .expect("RTMP runtimes were prepared before listener registration")
                    .clone();
                let mut service = Service::new(
                    service_name,
                    RtmpIngest::new(runtime, metrics.clone(), Arc::clone(generation)),
                );
                add_plain_listener(&mut service, &listener_name, &listener_bind)?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::Tcp(l4_service) => {
                let mut service = Service::new(
                    service_name,
                    TcpRelay::new(l4_service, metrics.clone())
                        .with_generation(Arc::clone(generation)),
                );
                add_plain_listener(&mut service, &listener_name, &listener_bind)?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
        }

        info!("configured {listener_name} on {listener_bind}");
    }

    let mut certbot_watcher = tls.start_certbot_watcher(CertbotWatcherConfig::default())?;
    runtime_metrics.register_certbot_monitoring(
        certbot_reconcilers,
        certbot_watcher
            .as_ref()
            .map(CertbotWatcherSupervisor::monitor),
    )?;
    setup
        .send(Ok(()))
        .map_err(|_| io::Error::other("generation setup receiver was dropped"))?;
    server.run(RunArgs {
        shutdown_signal: Box::new(ChannelShutdownSignal { shutdown }),
    });
    if let Some(watcher) = &mut certbot_watcher {
        watcher.shutdown();
    }
    Ok(())
}

fn unix_time_ms() -> Option<u64> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    u64::try_from(duration.as_millis()).ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use oxiroute_config::{
        Config, HealthCheck, HealthCheckType, HttpVersionPolicy, L4Service, Listener, ListenerBind,
        Protocol, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool,
    };
    use oxiroute_server::runtime_plan;
    use tokio::{net::TcpListener, sync::watch};

    use super::*;

    #[test]
    fn active_runtime_death_fails_the_generation_supervisor() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("oxiroute.lua");
        let config = Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        };
        fs::write(&path, oxiroute_config::render_lua(&config).expect("render")).expect("config");
        let coordinator = CanonicalConfigCoordinator::new(path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("load")
        };
        let manager = GenerationManager::new();
        let candidate = manager.prepare(*document).expect("candidate");
        let generation = manager.activate(&candidate).expect("active");
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let thread = thread::spawn(|| {});
        while !thread.is_finished() {
            thread::yield_now();
        }
        let process = GenerationProcess {
            generation,
            shutdown,
            thread,
        };
        let stop = AtomicBool::new(false);
        let process_shutdown = Arc::new(AtomicBool::new(false));

        assert!(!supervise_generations(
            &manager,
            &coordinator,
            process,
            &stop,
            &process_shutdown,
        ));
        assert!(process_shutdown.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn tcp_handler_closes_connections_when_its_pool_is_unavailable() {
        let ingress = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ingress bind");
        let ingress_address = ingress.local_addr().expect("ingress address");
        let config = Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: vec![Listener {
                name: "database".into(),
                bind: ListenerBind::Socket {
                    address: ingress_address,
                },
                protocol: Protocol::Tcp,
                service: Some("database".into()),
                tls_profile: None,
                max_connections: Some(10),
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
            upstream_pools: vec![UpstreamPool {
                name: "database".into(),
                servers: Vec::new(),
                endpoints: vec![UpstreamEndpoint::Socket {
                    address: "127.0.0.1:5432".parse().expect("upstream address"),
                }],
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: Some(HealthCheck {
                    kind: HealthCheckType::Tcp,
                    interval_ms: 1_000,
                    timeout_ms: 100,
                    healthy_threshold: 1,
                    unhealthy_threshold: 1,
                    startup: oxiroute_config::HealthStartup::default(),
                    fast_interval_ms: None,
                    down_interval_ms: None,
                    host: None,
                    path: None,
                    expected_status: None,
                    http_version: None,
                }),
                tls: None,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
            }],
            http_services: Vec::new(),
            cache_stores: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: vec![L4Service {
                name: "database".into(),
                upstream_pool: "database".into(),
                connect_timeout_ms: 100,
                idle_timeout_ms: 1_000,
                lifetime_timeout_ms: None,
            }],
        };
        let mut plan = runtime_plan(&config).expect("TCP runtime plan");
        let pool = Arc::clone(&plan.pools[0]);
        let spec = plan.services.remove(0);
        let metrics = RuntimeMetrics::new();
        let listener_metrics = metrics
            .register_configured_listener(
                &spec.name,
                spec.kind.protocol(),
                &spec.bind,
                spec.max_connections,
            )
            .expect("listener metrics");
        let ServiceKind::Tcp(service) = spec.kind else {
            panic!("listener must compile as TCP");
        };
        let app = Arc::new(TcpRelay::new(service, listener_metrics));
        let mut client = tokio::net::TcpStream::connect(ingress_address)
            .await
            .expect("client connect");
        let (downstream, _) = ingress.accept().await.expect("ingress accept");
        let downstream: Stream = Box::new(pingora::protocols::l4::stream::Stream::from(downstream));
        let (_shutdown_tx, shutdown) = watch::channel(false);
        let admission = app.admit_connection().expect("connection admission");

        assert!(app.process_new(downstream, &shutdown).await.is_none());
        drop(admission);
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.expect("client EOF");
        assert!(response.is_empty());
        assert_eq!(pool.health_snapshot().unavailable_selections, 1);
        let snapshot = metrics.snapshot().expect("runtime snapshot");
        assert_eq!(snapshot.traffic.accepted_connections, 1);
        assert_eq!(snapshot.traffic.active_connections, 0);
    }

    #[tokio::test]
    async fn rtmp_writer_accounts_for_bytes_written_to_a_real_socket() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("RTMP writer bind");
        let address = listener.local_addr().expect("RTMP writer address");
        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(address)
                .await
                .expect("RTMP writer client");
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.expect("RTMP bytes");
            bytes
        });
        let (server, _) = listener.accept().await.expect("RTMP writer accept");
        let mut server: Stream = Box::new(pingora::protocols::l4::stream::Stream::from(server));
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_listener("rtmp", "rtmp", address.to_string(), None)
            .expect("RTMP writer metrics");
        let packets = vec![b"first".to_vec(), b"-second".to_vec()];

        write_rtmp_packets(&mut server, &packets, &metrics)
            .await
            .expect("RTMP packet write");
        drop(server);

        assert_eq!(client.await.expect("RTMP client task"), b"first-second");
        assert_eq!(
            runtime
                .snapshot()
                .expect("RTMP writer snapshot")
                .traffic
                .bytes_sent,
            12
        );
    }

    struct ClosingApp;

    #[async_trait]
    impl ServerApp for ClosingApp {
        async fn process_new(
            self: &Arc<Self>,
            mut stream: Stream,
            _shutdown: &ShutdownWatch,
        ) -> Option<Stream> {
            let _ = stream.shutdown().await;
            None
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_reservation_drives_a_real_pingora_service_and_cleans_up() {
        use std::os::unix::fs::PermissionsExt as _;
        use tokio::net::UnixStream;

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("listener.sock");
        let bind = ListenerBind::Unix {
            path: path.clone(),
            mode: Some(0o640),
        };
        let reservation =
            ListenerReservation::bind("unix", &bind).expect("Unix listener reservation");
        assert_eq!(
            std::fs::symlink_metadata(&path)
                .expect("Unix socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_configured_listener("unix", "tcp", &bind, None)
            .expect("listener metrics");
        let mut service = Service::new("Unix listener test".into(), ClosingApp);
        add_plain_listener(&mut service, "unix", &bind).expect("Unix listener registration");
        let mut service = RuntimeListenerService::new(service, reservation, Some(metrics));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (ready_tx, mut ready) = watch::channel(false);
        let service_task = tokio::spawn(async move {
            service
                .start_service(None, shutdown, 1, ServiceReadyNotifier::new(ready_tx))
                .await;
        });

        ready.changed().await.expect("listener readiness");
        assert!(*ready.borrow());
        let mut client = UnixStream::connect(&path)
            .await
            .expect("connect to the real Unix listener");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("listener EOF");
        assert!(response.is_empty());
        assert_eq!(
            runtime.snapshot().expect("active snapshot").listeners[0].state,
            oxiroute_server::ListenerRuntimeState::Listening
        );

        shutdown_tx.send(true).expect("listener shutdown");
        service_task.await.expect("listener service task");
        assert_eq!(
            runtime.snapshot().expect("stopped snapshot").listeners[0].state,
            oxiroute_server::ListenerRuntimeState::Stopped
        );
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_listener_activation_rejects_and_preserves_an_existing_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("listener.sock");
        std::fs::write(&path, b"must remain").expect("existing path fixture");
        let bind = ListenerBind::Unix {
            path: path.clone(),
            mode: None,
        };
        let error = ListenerReservation::bind("unix", &bind)
            .err()
            .expect("existing path must make activation fail closed");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert_eq!(
            std::fs::read(path).expect("existing path remains"),
            b"must remain"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_listener_registration_rejects_non_utf8_paths_without_lossy_conversion() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let bind = ListenerBind::Unix {
            path: PathBuf::from(OsString::from_vec(vec![b'/', b'r', b'u', b'n', b'/', 0xff])),
            mode: None,
        };

        let error = ListenerReservation::bind("invalid", &bind)
            .err()
            .expect("non-UTF-8 Unix listener path must fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("not valid UTF-8"));
    }
}
