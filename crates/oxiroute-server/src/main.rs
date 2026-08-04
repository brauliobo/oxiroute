use std::{
    convert::Infallible,
    error::Error,
    future::Future as _,
    io,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    pin::Pin,
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use futures_util::FutureExt as _;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use log::{error, info, warn};
use oxiroute_config::ListenerBind;
use oxiroute_rtmp::{MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, RtmpRegistry, RtmpServiceRuntime};
use oxiroute_server::{
    AcmeManagedReconciler, CertbotWatcherConfig, CertbotWatcherSupervisor, ConfigWatcher,
    ConfigWatcherOptions, ConnectionGuard, FileWatcherConfig, FileWatcherSupervisor,
    ForwardConnectionLifecycle, ForwardHttp1ServicePlan, ForwardHttp2ServiceApp, GenerationManager,
    HaproxyStatsApi, HaproxyStatsPage, HttpDownstreamPolicyApp, HttpListenerApp, HttpReverseProxy,
    ListenerMetrics, ListenerReservation, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RtmpManagementApi,
    RuntimeGeneration, RuntimeMetrics, RuntimeReferenceKind, ServiceKind, TcpRelayCore,
    TlsProfilePlan, TopologySnapshot, UdpRuntime,
    cli::{Cli, Command, ConfigCommand, execute_offline},
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigRevision},
    emit_certificate,
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
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    time::{Instant, MissedTickBehavior, Sleep, interval, timeout},
};

#[cfg(target_os = "linux")]
mod supervised;

const RTMP_READ_BUFFER_SIZE: usize = 16 * 1024;
const RTMP_PLAYBACK_DRAIN_INTERVAL: Duration = Duration::from_millis(10);
const RTMP_PUBLISHER_LIVENESS_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const RTMP_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_LISTENER_DUPLICATION_FAILURE_ENV: &str =
    "OXIROUTE_INTERNAL_TEST_LISTENER_DUPLICATION_FAILURE";

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
        ListenerBind::Unix { path, mode } => {
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
                use std::os::unix::fs::PermissionsExt as _;

                service.add_uds(
                    path,
                    mode.map(|mode| std::fs::Permissions::from_mode(u32::from(mode))),
                );
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
        let service_name = self.inner.name().to_owned();
        let ready_notifier = ready_notifier.require_explicit();
        #[cfg(unix)]
        let duplicated = if cfg!(target_os = "linux")
            && std::env::var_os(TEST_LISTENER_DUPLICATION_FAILURE_ENV).is_some()
        {
            Err(io::Error::other("injected listener duplication failure"))
        } else {
            self.reservation.duplicate_fds()
        };
        #[cfg(unix)]
        let fds = match duplicated {
            Ok(fds) => fds,
            Err(error) => {
                if let Some(metrics) = &self.metrics {
                    metrics.mark_failed();
                }
                if let Some(generation) = &self.generation {
                    generation.mark_runtime_failed();
                }
                error!("listener service `{service_name}` descriptor duplication failed: {error}");
                return;
            }
        };
        #[cfg(unix)]
        let fds = Some(Arc::new(tokio::sync::Mutex::new(fds)));
        let ready_metrics = self.metrics.clone();
        let ready_notifier = ready_notifier.with_ready_callback(move || {
            if let Some(metrics) = ready_metrics {
                metrics.mark_listening();
            }
        });
        let result = AssertUnwindSafe(PingoraService::start_service_with_ready_notifier(
            &mut self.inner,
            #[cfg(unix)]
            fds,
            shutdown,
            listeners_per_fd,
            ready_notifier,
        ))
        .catch_unwind()
        .await;
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
        if !self.generation.mark_runtime_started() {
            return;
        }
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

struct ForwardHttp1App {
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    request_timeout: Option<Duration>,
    service: Arc<ForwardHttp1ServicePlan>,
    challenge_store: oxiroute_acme::ChallengeStore,
}

struct ForwardHttp2App<A> {
    generation: Arc<RuntimeGeneration>,
    inner: Arc<A>,
    metrics: ListenerMetrics,
    service: Arc<ForwardHttp1ServicePlan>,
}

impl<A> ForwardHttp2App<A> {
    fn new(
        service: Arc<ForwardHttp1ServicePlan>,
        inner: A,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            generation,
            inner: Arc::new(inner),
            metrics,
            service,
        }
    }
}

struct ForwardDownstream<S> {
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    inner: S,
    metrics: ConnectionGuard,
}

impl<S> ForwardDownstream<S> {
    fn new(inner: S, idle_timeout: Duration, metrics: ConnectionGuard) -> Self {
        Self {
            idle: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
            inner,
            metrics,
        }
    }

    fn reset_idle(&mut self) {
        self.idle.as_mut().reset(Instant::now() + self.idle_timeout);
    }

    fn poll_idle(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.idle.as_mut().poll(context).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "forward downstream idle timeout",
            ))
        } else {
            Ok(())
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ForwardDownstream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(context) {
            return Poll::Ready(Err(error));
        }
        let before = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(context, buffer);
        if let Poll::Ready(Ok(())) = &result {
            let read = buffer.filled().len() - before;
            if read > 0 {
                if let Err(error) = this
                    .metrics
                    .record_bytes_received(u64::try_from(read).unwrap_or(u64::MAX))
                {
                    return Poll::Ready(Err(io::Error::other(error)));
                }
                this.reset_idle();
            }
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ForwardDownstream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(context) {
            return Poll::Ready(Err(error));
        }
        let result = Pin::new(&mut this.inner).poll_write(context, buffer);
        if let Poll::Ready(Ok(written)) = result {
            if written > 0 {
                if let Err(error) = this
                    .metrics
                    .record_bytes_sent(u64::try_from(written).unwrap_or(u64::MAX))
                {
                    return Poll::Ready(Err(io::Error::other(error)));
                }
                this.reset_idle();
            }
            Poll::Ready(Ok(written))
        } else {
            result
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

impl ForwardHttp1App {
    fn new(
        service: Arc<ForwardHttp1ServicePlan>,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
        request_timeout: Option<Duration>,
        challenge_store: oxiroute_acme::ChallengeStore,
    ) -> Self {
        Self {
            generation,
            metrics,
            request_timeout,
            service,
            challenge_store,
        }
    }
}

#[async_trait]
impl ServerApp for ForwardHttp1App {
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.metrics.accepting() && self.generation.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        let generation = self
            .generation
            .begin_reference(RuntimeReferenceKind::ForwardHttp1)?;
        let connection = admit_connection(&self.metrics)?;
        Some(Box::new((service, generation, connection)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        let generation = self
            .generation
            .begin_owned_reference(RuntimeReferenceKind::ForwardHttp1);
        let connection = admit_connection(&self.metrics)?;
        Some(Box::new((service, generation, connection)))
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let client_addr = downstream.get_socket_digest().and_then(|digest| {
            digest
                .peer_addr()
                .and_then(|address| address.as_inet().copied())
        });
        let plan = Arc::clone(&self.service);
        let request_shutdown = shutdown.clone();
        let lifecycle = Arc::new(ForwardConnectionLifecycle::default());
        let request_lifecycle = Arc::clone(&lifecycle);
        let challenge_store = self.challenge_store.clone();
        let app = service_fn(move |request| {
            let plan = Arc::clone(&plan);
            let shutdown = request_shutdown.clone();
            let lifecycle = Arc::clone(&request_lifecycle);
            let challenge_store = challenge_store.clone();
            async move {
                let response = match oxiroute_server::challenge_response(&request, &challenge_store)
                {
                    Some(response) => response,
                    None => plan.handle(request, client_addr, shutdown, lifecycle).await,
                };
                Ok::<_, Infallible>(response)
            }
        });
        let mut builder = http1::Builder::new();
        builder.max_buf_size(self.service.max_header_bytes());
        let header_timeout = self
            .request_timeout
            .map_or(self.service.idle_timeout(), |timeout| {
                timeout.min(self.service.idle_timeout())
            });
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(header_timeout);
        let downstream = ForwardDownstream::new(
            downstream,
            self.service.idle_timeout(),
            self.metrics.traffic_accounting(),
        );
        let connection = builder
            .serve_connection(TokioIo::new(downstream), app)
            .with_upgrades();
        tokio::pin!(connection);
        let lifetime = tokio::time::sleep(self.service.lifetime_timeout());
        tokio::pin!(lifetime);
        let mut shutdown = shutdown.clone();
        tokio::select! {
            result = &mut connection => {
                if let Err(error) = result {
                    warn!("forward HTTP/1 connection failed: {error}");
                }
            }
            () = &mut lifetime => {}
            _ = shutdown.changed() => {}
        }
        lifecycle.wait_if_started().await;
        None
    }
}

#[async_trait]
impl<A> ServerApp for ForwardHttp2App<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.metrics.accepting() && self.generation.accepting() && self.inner.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        let generation = self
            .generation
            .begin_reference(RuntimeReferenceKind::Http2)?;
        let connection = admit_connection(&self.metrics)?;
        let inner = self.inner.admit_connection()?;
        Some(Box::new((service, generation, connection, inner)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        let generation = self
            .generation
            .begin_owned_reference(RuntimeReferenceKind::Http2);
        let connection = admit_connection(&self.metrics)?;
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((service, generation, connection, inner)))
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
        let mut publisher_liveness = interval(RTMP_PUBLISHER_LIVENESS_CHECK_INTERVAL);
        publisher_liveness.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let outbound = tokio::select! {
                _ = shutdown.changed() => break,
                _ = publisher_liveness.tick() => {
                    let Some(now_unix_ms) = unix_time_ms() else {
                        warn!("closing RTMP session because the system clock is invalid");
                        break;
                    };
                    at_unix_ms = now_unix_ms;
                    if session.is_publisher_stale(now_unix_ms) {
                        warn!("closing stale RTMP publisher session after media inactivity");
                        break;
                    }
                    Vec::new()
                }
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
    #[cfg(target_os = "linux")]
    if let Some(exit_code) = supervised::dispatch() {
        return exit_code;
    }

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

const ACME_RENEWAL_SCAN_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

struct AcmeManagedSupervisor {
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl AcmeManagedSupervisor {
    fn start(reconcilers: Vec<Arc<AcmeManagedReconciler>>) -> io::Result<Self> {
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("oxiroute-acme-renewal".into())
            .spawn(move || {
                loop {
                    let now = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .ok()
                        .map_or(0, |duration| duration.as_secs());
                    for reconciler in &reconcilers {
                        if !reconciler.renewal_due(now) {
                            continue;
                        }
                        let certificate = reconciler.status().certificate;
                        emit_certificate("certificate_renewal", "requested", &certificate);
                        match reconciler.renew_now() {
                            Ok(outcome) => {
                                emit_certificate(
                                    if outcome == oxiroute_server::AcmeManagedOutcome::Activated {
                                        "certificate_activated"
                                    } else {
                                        "certificate_renewal"
                                    },
                                    if outcome == oxiroute_server::AcmeManagedOutcome::Activated {
                                        "activated"
                                    } else {
                                        "applied"
                                    },
                                    &certificate,
                                );
                                info!(
                                    "managed ACME renewal for `{}` completed with {}",
                                    certificate,
                                    outcome.code()
                                );
                            }
                            Err(error) => {
                                emit_certificate("certificate_renewal", "failed", &certificate);
                                warn!(
                                    "managed ACME renewal for `{}` failed: {}",
                                    certificate,
                                    error.code()
                                );
                            }
                        }
                    }
                    if stop_rx
                        .recv_timeout(acme_scan_delay(&reconcilers, now))
                        .is_ok()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    fn shutdown(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn acme_scan_delay(reconcilers: &[Arc<AcmeManagedReconciler>], now: u64) -> Duration {
    reconcilers
        .iter()
        .filter_map(|reconciler| {
            let next = reconciler.status().next_action_unix_seconds?;
            (next > now).then(|| Duration::from_secs(next - now))
        })
        .min()
        .unwrap_or(ACME_RENEWAL_SCAN_INTERVAL)
        .min(ACME_RENEWAL_SCAN_INTERVAL)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenerationJoinOutcome {
    Joined,
    Panicked,
    Detached,
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
                &generation.revision().candidate.as_str()[..12]
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

    fn initiate_recorder_shutdown(
        &self,
        deadline: std::time::Instant,
    ) -> Vec<oxiroute_rtmp::RtmpRecorderShutdown> {
        self.generation.initiate_recorder_shutdown(deadline)
    }

    fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    fn join_until(self, deadline: std::time::Instant) -> GenerationJoinOutcome {
        self.request_shutdown();
        while !self.thread.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !self.thread.is_finished() {
            return GenerationJoinOutcome::Detached;
        }
        match self.thread.join() {
            Ok(()) => GenerationJoinOutcome::Joined,
            Err(_) => GenerationJoinOutcome::Panicked,
        }
    }
}

fn finish_generation_process(process: GenerationProcess, deadline: std::time::Instant) -> bool {
    match process.join_until(deadline) {
        GenerationJoinOutcome::Joined => true,
        GenerationJoinOutcome::Panicked => {
            error!("generation runtime thread panicked during shutdown");
            false
        }
        GenerationJoinOutcome::Detached => {
            warn!("generation runtime did not stop before the process shutdown deadline");
            true
        }
    }
}

fn push_unique_recorder_shutdown(
    shutdowns: &mut Vec<oxiroute_rtmp::RtmpRecorderShutdown>,
    shutdown: oxiroute_rtmp::RtmpRecorderShutdown,
) {
    if !shutdowns
        .iter()
        .any(|existing| existing.is_same_lifecycle(&shutdown))
    {
        shutdowns.push(shutdown);
    }
}

fn shutdown_generation_processes(
    manager: &GenerationManager,
    processes: Vec<GenerationProcess>,
    deadline: std::time::Instant,
) -> bool {
    let mut recorder_shutdowns = manager.begin_shutdown(deadline);
    for process in &processes {
        for shutdown in process.initiate_recorder_shutdown(deadline) {
            push_unique_recorder_shutdown(&mut recorder_shutdowns, shutdown);
        }
        process.generation.stop_accepting();
    }
    while std::time::Instant::now() < deadline
        && processes
            .iter()
            .any(|process| !process.generation.drained())
    {
        thread::sleep(Duration::from_millis(10));
    }
    for process in &processes {
        process.request_shutdown();
    }
    for shutdown in &recorder_shutdowns {
        if !shutdown.wait_until(deadline) {
            break;
        }
    }
    let mut clean = true;
    for process in processes {
        clean &= finish_generation_process(process, deadline);
    }
    clean
}

fn supervise_generations(
    manager: &GenerationManager,
    coordinator: &CanonicalConfigCoordinator,
    mut current: GenerationProcess,
    stop: &AtomicBool,
    process_shutdown: &Arc<AtomicBool>,
) -> bool {
    let mut retired = Vec::<GenerationProcess>::new();
    let mut successful = true;
    while !stop.load(Ordering::Acquire) {
        if current.is_finished() || current.generation.runtime_failed() {
            error!("active generation runtime terminated unexpectedly");
            process_shutdown.store(true, Ordering::Release);
            successful = false;
            break;
        }
        retired = retired
            .into_iter()
            .filter_map(|process| {
                if process.generation.drained() {
                    process.request_shutdown();
                }
                if process.is_finished() {
                    successful &= finish_generation_process(process, std::time::Instant::now());
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
        let mut startup = match manager.begin_candidate_start(&candidate) {
            Ok(startup) => startup,
            Err(oxiroute_server::GenerationError::MutationInProgress) => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => continue,
        };
        let starting_generation = match startup.claim_runtime_start() {
            Ok(generation) => generation,
            Err(error) => {
                error!("candidate generation start claim failed: {error}");
                continue;
            }
        };

        match GenerationProcess::start(
            starting_generation,
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
                    successful &= finish_generation_process(
                        next,
                        std::time::Instant::now() + Duration::from_secs(5),
                    );
                }
            },
            Err(error) => {
                error!("candidate generation could not start: {error}");
                manager.quarantine(&candidate, "runtime_start");
            }
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    retired.push(current);
    let shutdown_clean = shutdown_generation_processes(manager, retired, deadline);
    successful && shutdown_clean
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

fn start_config_watcher_or_shutdown<F>(
    coordinator: CanonicalConfigCoordinator,
    manager: &GenerationManager,
    initial: &Arc<Mutex<Option<GenerationProcess>>>,
    start: F,
) -> Result<ConfigWatcher, Box<dyn Error>>
where
    F: FnOnce(
        CanonicalConfigCoordinator,
        GenerationManager,
        ConfigWatcherOptions,
    ) -> notify::Result<ConfigWatcher>,
{
    match start(
        coordinator,
        manager.clone(),
        ConfigWatcherOptions::default(),
    ) {
        Ok(watcher) => Ok(watcher),
        Err(error) => {
            let process = initial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(process) = process {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let _ = shutdown_generation_processes(manager, vec![process], deadline);
            } else {
                error!("initial generation ownership was lost before watcher failure cleanup");
            }
            Err(error.into())
        }
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
    let mut startup = generation_manager.begin_candidate_start(&candidate)?;
    let starting_generation = startup.claim_runtime_start()?;
    let initial = GenerationProcess::start(
        starting_generation,
        config_coordinator.clone(),
        generation_manager.clone(),
        &stop_supervisor,
    )?;
    if let Err(error) = wait_for_generation_ready(candidate.generation(), &stop_supervisor) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let _ = shutdown_generation_processes(&generation_manager, vec![initial], deadline);
        signal_handle.close();
        let _ = signal_thread.join();
        return Err(error);
    }
    let supervisor_stop = Arc::clone(&stop_supervisor);
    let supervisor_manager = generation_manager.clone();
    let supervisor_coordinator = config_coordinator.clone();
    let supervisor_healthy = Arc::new(AtomicBool::new(true));
    let thread_supervisor_healthy = Arc::clone(&supervisor_healthy);
    let initial = Arc::new(Mutex::new(Some(initial)));
    let thread_initial = Arc::clone(&initial);
    let (start_supervisor, await_supervisor_start) = mpsc::sync_channel(1);
    let supervisor = match thread::Builder::new()
        .name("oxiroute-generation-supervisor".into())
        .spawn(move || {
            if await_supervisor_start.recv().is_err() {
                return;
            }
            let Some(initial) = thread_initial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            else {
                thread_supervisor_healthy.store(false, Ordering::Release);
                return;
            };
            if !supervise_generations(
                &supervisor_manager,
                &supervisor_coordinator,
                initial,
                &supervisor_stop,
                &supervisor_stop,
            ) {
                thread_supervisor_healthy.store(false, Ordering::Release);
            }
        }) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            let initial = initial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(initial) = initial {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let _ = shutdown_generation_processes(&generation_manager, vec![initial], deadline);
            }
            signal_handle.close();
            let _ = signal_thread.join();
            return Err(error.into());
        }
    };
    if let Err(error) = startup.activate() {
        drop(start_supervisor);
        let _ = supervisor.join();
        let initial = initial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(initial) = initial {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let _ = shutdown_generation_processes(&generation_manager, vec![initial], deadline);
        }
        signal_handle.close();
        let _ = signal_thread.join();
        return Err(error.into());
    }
    let mut config_watcher = match start_config_watcher_or_shutdown(
        config_coordinator,
        &generation_manager,
        &initial,
        ConfigWatcher::start,
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            drop(start_supervisor);
            let _ = supervisor.join();
            signal_handle.close();
            let _ = signal_thread.join();
            return Err(error);
        }
    };
    if start_supervisor.send(()).is_err() {
        config_watcher.shutdown();
        let initial = initial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(initial) = initial {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let _ = shutdown_generation_processes(&generation_manager, vec![initial], deadline);
        }
        let _ = supervisor.join();
        signal_handle.close();
        let _ = signal_thread.join();
        return Err(io::Error::other("generation supervisor terminated before startup").into());
    }
    while !stop_supervisor.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(20));
    }
    signal_handle.close();
    let signal_result = signal_thread.join();
    config_watcher.shutdown();
    let supervisor_result = supervisor.join();
    if signal_result.is_err() {
        return Err(io::Error::other("signal thread terminated unexpectedly").into());
    }
    if supervisor_result.is_err() {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        for shutdown in generation_manager.begin_shutdown(deadline) {
            if !shutdown.wait_until(deadline) {
                break;
            }
        }
        return Err(io::Error::other("generation supervisor terminated unexpectedly").into());
    }
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
    let active_revision = generation.revision().candidate.clone();
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
    let mut server = Server::new_with_opt_and_conf(None, server_config)?;
    server.bootstrap();
    server.add_service(GenerationRuntimeMarker {
        generation: Arc::clone(generation),
    });

    let plan = generation.plan();
    let services = plan.services.clone();
    let health_supervisor = plan.health_supervisor.clone();
    let pools = plan.pools.clone();
    let tls = &plan.tls;
    let challenge_store = tls.challenge_store().clone();
    let topology = Arc::clone(&plan.topology);
    let acme_reconcilers = tls.acme_reconcilers().to_vec();
    let certbot_reconcilers = tls.certbot_reconcilers().to_vec();
    let direct_file_reconcilers = tls.file_reconcilers().to_vec();
    let mut udp_runtimes = Vec::new();
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
                .management()
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
            HttpListenerApp::new(management_api.into_http_app(), None)
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
                .stats(index)
                .cloned()
                .expect("candidate reserved every statistics listener");
            server.add_service(
                RuntimeListenerService::new(service, reservation, None)
                    .with_generation(Arc::clone(generation)),
            );
            info!("configured HAProxy-compatible statistics on {bind}");
        }
        for (index, page) in stats.pages.iter().enumerate() {
            let app = HaproxyStatsPage::new(page, pools.clone(), generation_manager.clone());
            let listener_name = format!("@stats-page-{index}");
            let bind = ListenerBind::Socket { address: page.bind };
            let metrics = runtime_metrics.register_configured_listener(
                &listener_name,
                "http",
                &bind,
                page.max_connections,
            )?;
            let stats_app = MonitoredHttpApp::new(
                HttpListenerApp::new(
                    HttpDownstreamPolicyApp::new(
                        HttpServer::new_app(app),
                        page.downstream_timeouts,
                    ),
                    None,
                )
                .with_generation(Arc::clone(generation)),
                metrics.clone(),
            )
            .with_generation(Arc::clone(generation));
            let mut service = Service::new(format!("OxiRoute stats page {index}"), stats_app);
            service.add_tcp(&page.bind.to_string());
            let reservation = generation
                .reservations()
                .stats_page(index)
                .cloned()
                .expect("candidate reserved every statistics page listener");
            server.add_service(
                RuntimeListenerService::new(service, reservation, Some(metrics))
                    .with_generation(Arc::clone(generation)),
            );
            info!(
                "configured HAProxy-compatible statistics page on {}",
                page.bind
            );
        }
    }

    for (spec, metrics, reservation) in services {
        let listener_name = spec.name;
        let listener_bind = spec.bind;
        let service_name = format!("OxiRoute {listener_name}");
        let listener_tls = spec.tls;
        let downstream_timeouts = spec.downstream_timeouts;

        match spec.kind {
            ServiceKind::ForwardHttp1(forward_service) => {
                let mut service = Service::new(
                    service_name,
                    ForwardHttp1App::new(
                        forward_service,
                        metrics.clone(),
                        Arc::clone(generation),
                        downstream_timeouts
                            .request_timeout_ms
                            .map(Duration::from_millis),
                        challenge_store.clone(),
                    ),
                );
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
            ServiceKind::ForwardHttp2(forward_service) => {
                let app = ForwardHttp2App::new(
                    Arc::clone(&forward_service),
                    HttpListenerApp::new(
                        HttpDownstreamPolicyApp::new(
                            ForwardHttp2ServiceApp::new(forward_service),
                            downstream_timeouts,
                        ),
                        listener_tls.as_deref(),
                    ),
                    metrics.clone(),
                    Arc::clone(generation),
                );
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
            ServiceKind::Http(http_service) => {
                let proxy = http_proxy(
                    &server.configuration,
                    HttpReverseProxy::new(http_service, metrics.clone())
                        .with_challenge_store(challenge_store.clone())
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
            ServiceKind::Udp(l4_service) => {
                let runtime = UdpRuntime::start(
                    listener_name.clone(),
                    reservation,
                    l4_service,
                    Arc::clone(generation),
                    metrics,
                    shutdown.clone(),
                )
                .map_err(|error| {
                    generation.mark_runtime_failed();
                    error
                })?;
                udp_runtimes.push(runtime);
            }
        }

        info!("configured {listener_name} on {listener_bind}");
    }

    let mut certbot_watcher = tls.start_certbot_watcher(CertbotWatcherConfig::default())?;
    runtime_metrics.register_acme_managed_monitoring(acme_reconcilers.clone())?;
    runtime_metrics.register_certbot_monitoring(
        certbot_reconcilers,
        certbot_watcher
            .as_ref()
            .map(CertbotWatcherSupervisor::monitor),
    )?;
    let mut direct_file_watcher = tls.start_file_watcher(FileWatcherConfig::default())?;
    runtime_metrics.register_direct_file_monitoring(
        direct_file_reconcilers,
        direct_file_watcher
            .as_ref()
            .map(FileWatcherSupervisor::monitor),
    )?;
    setup
        .send(Ok(()))
        .map_err(|_| io::Error::other("generation setup receiver was dropped"))?;
    let acme_supervisor = AcmeManagedSupervisor::start(acme_reconcilers)?;
    server.run(RunArgs {
        shutdown_signal: Box::new(ChannelShutdownSignal { shutdown }),
    });
    for runtime in udp_runtimes {
        runtime.join()?;
    }
    acme_supervisor.shutdown();
    if let Some(watcher) = &mut certbot_watcher {
        watcher.shutdown();
    }
    if let Some(watcher) = &mut direct_file_watcher {
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
    use std::{collections::VecDeque, fs};

    use bytes::Bytes;
    use oxiroute_config::{
        Config, HealthCheck, HealthCheckType, HttpVersionPolicy, L4Service, Listener, ListenerBind,
        Protocol, RtmpApplication, RtmpRecorder, RtmpRecorderStart, RtmpService, UpstreamAlgorithm,
        UpstreamEndpoint, UpstreamPool,
    };
    use oxiroute_rtmp::RtmpSession;
    use oxiroute_server::runtime_plan;
    use rml_rtmp::{
        handshake::{Handshake, HandshakeProcessResult, PeerType},
        sessions::{
            ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
            PublishRequestType,
        },
        time::RtmpTimestamp,
    };
    use rustix::fs::{FlockOperation, flock};
    use tokio::{net::TcpListener, sync::watch};

    use super::*;

    #[test]
    fn active_runtime_death_fails_the_generation_supervisor() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("oxiroute.kdl");
        let recording_root = directory.path().join("recordings");
        fs::create_dir(&recording_root).expect("recording root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener address");
        let listener_address = listener.local_addr().expect("listener address");
        drop(listener);
        let config = recorder_runtime_config(listener_address, &recording_root);
        let (coordinator, manager, generation) = activate_test_generation(&path, &config);
        let ownership = fs::File::open(&recording_root).expect("recording root ownership");
        flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
        publish_stalled_recorder(&generation);
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
        let started = std::time::Instant::now();

        assert!(!supervise_generations(
            &manager,
            &coordinator,
            process,
            &stop,
            &process_shutdown,
        ));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500),
            "runtime failure bypassed bounded recorder shutdown: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(5));
        assert!(process_shutdown.load(Ordering::Acquire));
        flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
    }

    #[test]
    fn post_activation_watcher_failure_runs_bounded_recorder_cleanup() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("oxiroute.kdl");
        let recording_root = directory.path().join("recordings");
        fs::create_dir(&recording_root).expect("recording root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener address");
        let listener_address = listener.local_addr().expect("listener address");
        drop(listener);
        let config = recorder_runtime_config(listener_address, &recording_root);
        let (coordinator, manager, generation) = activate_test_generation(&path, &config);
        let ownership = fs::File::open(&recording_root).expect("recording root ownership");
        flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let thread = thread::spawn(|| {});
        while !thread.is_finished() {
            thread::yield_now();
        }
        let initial = Arc::new(Mutex::new(Some(GenerationProcess {
            generation: Arc::clone(&generation),
            shutdown,
            thread,
        })));
        let started = std::time::Instant::now();

        let result =
            start_config_watcher_or_shutdown(coordinator, &manager, &initial, move |_, _, _| {
                publish_stalled_recorder(&generation);
                Err(notify::Error::generic("injected watcher failure"))
            });
        let elapsed = started.elapsed();

        assert!(result.is_err());
        assert!(initial.lock().expect("initial process slot").is_none());
        assert!(
            elapsed >= Duration::from_millis(500),
            "watcher failure bypassed recorder cleanup: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(5));
        flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
    }

    #[test]
    fn generation_join_detaches_at_the_absolute_process_deadline() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("oxiroute.kdl");
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
        let (_, manager, generation) = activate_test_generation(&path, &config);
        let (release, released) = mpsc::sync_channel(0);
        let (finished, completion) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            released.recv().expect("release stuck generation");
            finished.send(()).expect("generation completion receiver");
        });
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let process = GenerationProcess {
            generation,
            shutdown,
            thread,
        };
        let started = std::time::Instant::now();

        let outcome = process.join_until(started + Duration::from_millis(75));
        let elapsed = started.elapsed();

        assert_eq!(outcome, GenerationJoinOutcome::Detached);
        assert!(elapsed >= Duration::from_millis(50));
        assert!(elapsed < Duration::from_millis(250));
        release.send(()).expect("release generation thread");
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("detached generation completion");

        let thread = thread::spawn(|| panic!("injected completed generation panic"));
        while !thread.is_finished() {
            thread::yield_now();
        }
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let completed = GenerationProcess {
            generation: manager.active().expect("active generation"),
            shutdown,
            thread,
        };
        assert_eq!(
            completed.join_until(std::time::Instant::now()),
            GenerationJoinOutcome::Panicked
        );
    }

    fn activate_test_generation(
        path: &Path,
        config: &Config,
    ) -> (
        CanonicalConfigCoordinator,
        GenerationManager,
        Arc<RuntimeGeneration>,
    ) {
        fs::write(
            path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Kdl,
                config,
            )
            .expect("render"),
        )
        .expect("config");
        let coordinator = CanonicalConfigCoordinator::new(path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("load")
        };
        let manager = GenerationManager::new();
        let candidate = manager.prepare(*document).expect("candidate");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        assert!(generation.mark_runtime_started());
        startup.activate().expect("active");
        (coordinator, manager, generation)
    }

    fn publish_stalled_recorder(generation: &RuntimeGeneration) {
        let mut server = generation
            .rtmp_runtime("live")
            .expect("RTMP runtime")
            .session();
        let mut client = connect_rtmp_session(&mut server, "live");
        let publish = client
            .request_publishing("camera".into(), PublishRequestType::Live)
            .expect("publish request");
        let events = exchange_rtmp(&mut client, &mut server, vec![publish], 1_000);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
        );
        let audio = client
            .publish_audio_data(
                Bytes::from_static(&[0xaf, 0x01, 0x44]),
                RtmpTimestamp::new(1),
                false,
            )
            .expect("audio packet");
        exchange_rtmp(&mut client, &mut server, vec![audio], 1_001);
        thread::sleep(Duration::from_millis(50));
    }

    fn recorder_runtime_config(
        listener_address: std::net::SocketAddr,
        recording_root: &Path,
    ) -> Config {
        Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: vec![Listener {
                name: "live".into(),
                bind: ListenerBind::Socket {
                    address: listener_address,
                },
                protocol: Protocol::Rtmp,
                service: Some("live".into()),
                tls_profile: None,
                max_connections: Some(4),
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: vec![RtmpService {
                name: "live".into(),
                outbound_chunk_size: 4_096,
                access_log: None,
                applications: vec![RtmpApplication {
                    name: "live".into(),
                    live: true,
                    idle_streams: true,
                    push_targets: Vec::new(),
                    fanout: oxiroute_config::RtmpFanoutPolicy::default(),
                    recorders: vec![RtmpRecorder {
                        name: "archive".into(),
                        start: RtmpRecorderStart::Continuous,
                        root_directory: recording_root.to_path_buf(),
                        suffix_template: ".flv".into(),
                        append_unix_seconds: false,
                        timezone: oxiroute_config::RtmpRecorderTimezone::default(),
                        time_basis: oxiroute_config::RtmpRecorderTimeBasis::default(),
                        segment_naming: oxiroute_config::RtmpRecorderSegmentNaming::default(),
                        rotation_interval_ms: None,
                        max_queue_messages: 32,
                        max_queue_bytes: 1_024,
                        shutdown_timeout_ms: 1_000,
                        max_storage_bytes: Some(1024 * 1024),
                        max_storage_files: Some(32),
                        max_active_recorders: 4,
                    }],
                }],
            }],
            l4_services: Vec::new(),
        }
    }

    fn connect_rtmp_session(server: &mut RtmpSession, application: &str) -> ClientSession {
        let mut handshake = Handshake::new(PeerType::Client);
        let client_hello = handshake
            .generate_outbound_p0_and_p1()
            .expect("client hello");
        let server_hello = server.receive(&client_hello, 1_000).expect("server hello");
        let client_finish = match handshake
            .process_bytes(&server_hello.concat())
            .expect("client handshake response")
        {
            HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
            result @ HandshakeProcessResult::InProgress { .. } => {
                panic!("client handshake did not complete: {result:?}");
            }
        };
        let startup = server
            .receive(&client_finish, 1_000)
            .expect("server handshake completion");
        let (mut client, initial) = ClientSession::new(ClientSessionConfig::new()).expect("client");
        assert!(initial.is_empty());
        assert!(feed_rtmp_client(&mut client, startup).0.is_empty());
        let request = client
            .request_connection(application.into())
            .expect("connection request");
        let events = exchange_rtmp(&mut client, server, vec![request], 1_000);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
        );
        client
    }

    fn exchange_rtmp(
        client: &mut ClientSession,
        server: &mut RtmpSession,
        initial: Vec<ClientSessionResult>,
        at_unix_ms: u64,
    ) -> Vec<ClientSessionEvent> {
        let mut packets = outbound_rtmp_packets(initial);
        let mut events = Vec::new();
        for _ in 0..8 {
            if packets.is_empty() {
                return events;
            }
            let mut responses = Vec::new();
            while let Some(packet) = packets.pop_front() {
                responses.extend(server.receive(&packet, at_unix_ms).expect("server input"));
            }
            let (next, mut raised) = feed_rtmp_client(client, responses);
            packets = next;
            events.append(&mut raised);
        }
        panic!("RTMP exchange did not settle");
    }

    fn feed_rtmp_client(
        client: &mut ClientSession,
        server_packets: Vec<Vec<u8>>,
    ) -> (VecDeque<Vec<u8>>, Vec<ClientSessionEvent>) {
        let mut packets = VecDeque::new();
        let mut events = Vec::new();
        for packet in server_packets {
            for result in client.handle_input(&packet).expect("client input") {
                match result {
                    ClientSessionResult::OutboundResponse(packet) => {
                        packets.push_back(packet.bytes);
                    }
                    ClientSessionResult::RaisedEvent(event) => events.push(event),
                    ClientSessionResult::UnhandleableMessageReceived(_) => {}
                }
            }
        }
        (packets, events)
    }

    fn outbound_rtmp_packets(results: Vec<ClientSessionResult>) -> VecDeque<Vec<u8>> {
        results
            .into_iter()
            .filter_map(|result| match result {
                ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
                ClientSessionResult::RaisedEvent(_)
                | ClientSessionResult::UnhandleableMessageReceived(_) => None,
            })
            .collect()
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
                udp: None,
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

    struct PanickingService;

    #[async_trait]
    impl PingoraService for PanickingService {
        async fn start_service_with_ready_notifier(
            &mut self,
            #[cfg(unix)] _fds: Option<pingora::server::ListenFds>,
            _shutdown: ShutdownWatch,
            _listeners_per_fd: usize,
            _ready_notifier: ServiceReadyNotifier,
        ) {
            panic!("injected listener startup panic");
        }

        fn name(&self) -> &'static str {
            "Panicking listener"
        }
    }

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
    #[tokio::test]
    async fn listener_build_error_never_notifies_ready_and_marks_metrics_failed() {
        let reservation = ListenerReservation::bind(
            "reserved",
            &ListenerBind::Socket {
                address: "127.0.0.1:0".parse().expect("reservation address"),
            },
        )
        .expect("listener reservation");
        let bind = ListenerBind::Socket {
            address: "127.0.0.1:0".parse().expect("metrics address"),
        };
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_configured_listener("mismatch", "tcp", &bind, None)
            .expect("listener metrics");
        let mut service = Service::new("Listener build failure".into(), ClosingApp);
        service.add_tcp("not-a-listener-address");
        let mut service = RuntimeListenerService::new(service, reservation, Some(metrics));
        let (_shutdown_tx, shutdown) = watch::channel(false);
        let (ready_tx, ready) = watch::channel(false);

        service
            .start_service(None, shutdown, 1, ServiceReadyNotifier::new(ready_tx))
            .await;

        assert!(!*ready.borrow(), "failed listener build signaled readiness");
        assert_eq!(
            runtime.snapshot().expect("failed snapshot").listeners[0].state,
            oxiroute_server::ListenerRuntimeState::Failed
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn listener_startup_panic_is_caught_without_notifying_ready() {
        let bind = ListenerBind::Socket {
            address: "127.0.0.1:0".parse().expect("reservation address"),
        };
        let reservation = ListenerReservation::bind("panic", &bind).expect("listener reservation");
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_configured_listener("panic", "tcp", &bind, None)
            .expect("listener metrics");
        let mut service = RuntimeListenerService::new(PanickingService, reservation, Some(metrics));
        let (_shutdown_tx, shutdown) = watch::channel(false);
        let (ready_tx, ready) = watch::channel(false);

        service
            .start_service(None, shutdown, 1, ServiceReadyNotifier::new(ready_tx))
            .await;

        assert!(
            !*ready.borrow(),
            "panicked listener startup signaled readiness"
        );
        assert_eq!(
            runtime.snapshot().expect("failed snapshot").listeners[0].state,
            oxiroute_server::ListenerRuntimeState::Failed
        );
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
