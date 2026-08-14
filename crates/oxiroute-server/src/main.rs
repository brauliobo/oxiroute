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

#[cfg(test)]
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use futures_util::FutureExt as _;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use log::{debug, error, info, warn};
use oxiroute_acme::{AcmeOperation, Dns01Cancellation};
use oxiroute_config::ListenerBind;
use oxiroute_rtmp::{
    MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, MediaCatalog, RecorderErrorCode, RecorderPhase,
    RtmpClientSnapshot, RtmpRecorderLifecycle, RtmpRecorderShutdown, RtmpRegistry,
    RtmpRelayFailure, RtmpServiceRuntime, RtmpSessionError, RtmpSessionRole, VodCatalog,
};
use oxiroute_server::{
    AcmeManagedReconciler, CertbotWatcherConfig, CertbotWatcherSupervisor, ConfigWatcher,
    ConfigWatcherOptions, ConnectionGuard, FileWatcherConfig, FileWatcherSupervisor,
    ForwardConnectionLifecycle, ForwardHttp1ServicePlan, ForwardHttp2ServiceApp, GenerationManager,
    HaproxyStatsApi, HaproxyStatsPage, Http3Runtime, HttpDownstreamPolicyApp, HttpListenerApp,
    HttpReverseProxy, ListenerMetrics, ListenerReservation, MAX_HTTP_ATTEMPTS, MonitoredHttpApp,
    RtmpManagementApi, RtmpServicePlan, RuntimeGeneration, RuntimeMetrics, RuntimeReferenceKind,
    ServiceKind, TcpRelayCore, TlsProfilePlan, TopologySnapshot, UdpRuntime,
    cli::{Cli, Command, ConfigCommand, execute_offline},
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, EffectiveRevision},
    emit_certificate, emit_rtmp_access,
};
use pingora::{
    apps::http_app::HttpServer,
    apps::{AcceptGate, ConnectionAdmission, ServerApp},
    protocols::{ALPN, Stream},
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
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
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
const RTMP_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RTMP_PUBLISHER_LIVENESS_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const RTMP_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const GENERATION_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
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

struct GenerationBackgroundService<S> {
    _generation: Arc<RuntimeGeneration>,
    inner: S,
}

#[async_trait]
impl<S> pingora::services::background::BackgroundService for GenerationBackgroundService<S>
where
    S: pingora::services::background::BackgroundService + Sync,
{
    async fn start_with_ready_notifier(
        &self,
        shutdown: ShutdownWatch,
        ready_notifier: ServiceReadyNotifier,
    ) {
        self.inner
            .start_with_ready_notifier(shutdown, ready_notifier)
            .await;
    }
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
    metrics: ListenerMetrics,
}

impl<A> ProcessAdmissionApp<A> {
    fn new(inner: A, metrics: ListenerMetrics, generation: Arc<RuntimeGeneration>) -> Self {
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
        let connection = match self.metrics.begin_control_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected management connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((
            generation_admission,
            generation_reference,
            connection,
            inner,
        )))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let generation_reference = self
            .generation
            .begin_owned_reference(RuntimeReferenceKind::Http1);
        let connection = match self.metrics.begin_control_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected management connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((generation_reference, connection, inner)))
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
    proxy_protocol: Option<oxiroute_config::ProxyProtocolPolicy>,
}

struct ForwardHttp1App {
    metrics: ListenerMetrics,
    require_h1_alpn: bool,
    request_timeout: Option<Duration>,
    service: Arc<ForwardHttp1ServicePlan>,
    challenge_store: oxiroute_acme::ChallengeStore,
}

struct ForwardHttp1RuntimeApp {
    inner: Arc<MonitoredHttpApp<ForwardHttp1App>>,
    handshake_timeout: Duration,
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
        request_timeout: Option<Duration>,
        require_h1_alpn: bool,
        challenge_store: oxiroute_acme::ChallengeStore,
    ) -> Self {
        Self {
            metrics,
            require_h1_alpn,
            request_timeout,
            service,
            challenge_store,
        }
    }
}

impl ForwardHttp1RuntimeApp {
    fn new(
        inner: ForwardHttp1App,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        let handshake_timeout = inner.handshake_timeout();
        Self {
            inner: Arc::new(MonitoredHttpApp::new(inner, metrics).with_generation(generation)),
            handshake_timeout,
        }
    }
}

#[async_trait]
impl ServerApp for ForwardHttp1App {
    fn handshake_timeout(&self) -> Duration {
        self.service
            .idle_timeout()
            .min(self.service.lifetime_timeout())
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        Some(Box::new(service))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        Some(Box::new(service))
    }

    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        if self.require_h1_alpn && !matches!(downstream.selected_alpn_proto(), Some(ALPN::H1)) {
            // A TLS forward-HTTP/1 listener must not accept an unnegotiated protocol.
            let _ = downstream.shutdown().await;
            return None;
        }
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
        let request_metrics = self.metrics.clone();
        let app = service_fn(move |request| {
            let plan = Arc::clone(&plan);
            let shutdown = request_shutdown.clone();
            let lifecycle = Arc::clone(&request_lifecycle);
            let challenge_store = challenge_store.clone();
            let metrics = request_metrics.clone();
            async move {
                let response = match oxiroute_server::challenge_response(&request, &challenge_store)
                {
                    Some(response) => response,
                    None => {
                        plan.handle(request, client_addr, shutdown, lifecycle, metrics)
                            .await
                    }
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
impl ServerApp for ForwardHttp1RuntimeApp {
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.inner.accept_gate()
    }

    fn accepting(&self) -> bool {
        self.inner.accepting()
    }

    fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_connection()
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_owned_connection()
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
    fn new(
        service: Arc<oxiroute_server::L4ServicePlan>,
        metrics: ListenerMetrics,
        proxy_protocol: Option<oxiroute_config::ProxyProtocolPolicy>,
    ) -> Self {
        Self {
            generation: None,
            service,
            metrics,
            proxy_protocol,
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
        let physical_client_address = downstream.get_socket_digest().and_then(|digest| {
            digest
                .peer_addr()
                .and_then(|address| address.as_inet().copied())
        });
        let connection = self.metrics.traffic_accounting();
        if let Some(policy) = self.proxy_protocol {
            let mut proxy_shutdown = shutdown.clone();
            let accepted =
                match oxiroute_server::accept_stream(downstream, policy, &mut proxy_shutdown).await
                {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        if let Err(metric_error) =
                            self.metrics.record_proxy_protocol(error.result())
                        {
                            debug!(
                                "could not account for TCP PROXY protocol rejection: {metric_error}"
                            );
                        }
                        warn!("TCP PROXY protocol rejected: {error}");
                        return None;
                    }
                };
            if let Err(metric_error) = self
                .metrics
                .record_proxy_protocol(oxiroute_server::ProxyProtocolResult::Accepted)
            {
                debug!("could not account for TCP PROXY protocol: {metric_error}");
            }
            let Some(upstream) =
                oxiroute_server::select_upstream_with_shutdown(&self.service, shutdown).await
            else {
                warn!("TCP pool has no healthy upstream");
                return None;
            };
            let relay = TcpRelayCore::new(upstream, self.service.policy())
                .with_proxy_protocol(self.service.proxy_protocol(), Some(accepted.header.source));
            if let Err(error) = relay
                .relay(accepted.stream, &connection, shutdown.clone())
                .await
            {
                warn!("TCP relay failed: {error}");
            }
        } else {
            let Some(upstream) =
                oxiroute_server::select_upstream_with_shutdown(&self.service, shutdown).await
            else {
                warn!("TCP pool has no healthy upstream");
                return None;
            };
            let relay = TcpRelayCore::new(upstream, self.service.policy())
                .with_proxy_protocol(self.service.proxy_protocol(), physical_client_address);
            if let Err(error) = relay.relay(downstream, &connection, shutdown.clone()).await {
                warn!("TCP relay failed: {error}");
            }
        }

        None
    }
}

struct RtmpIngest {
    listener: String,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    runtime: RtmpServiceRuntime,
    service: Arc<RtmpServicePlan>,
}

impl RtmpIngest {
    fn new(
        listener: String,
        runtime: RtmpServiceRuntime,
        service: Arc<RtmpServicePlan>,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            listener,
            generation,
            metrics,
            runtime,
            service,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RtmpAccessEvent {
    Connect,
    Disconnect,
    Publish,
    Play,
    Record,
    Relay,
}

impl RtmpAccessEvent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
            Self::Publish => "publish",
            Self::Play => "play",
            Self::Record => "record",
            Self::Relay => "relay",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RtmpAccessResult {
    Accepted,
    Rejected,
    Closed,
    Failed,
}

impl RtmpAccessResult {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RtmpAccessCounters {
    bytes_received: u64,
    bytes_sent: u64,
    messages_received: u64,
    messages_sent: u64,
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

    #[allow(clippy::too_many_lines)]
    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let Some(mut at_unix_ms) = unix_time_ms() else {
            warn!("cannot start RTMP session because the system clock is invalid");
            return None;
        };
        let session_started_at = Instant::now();
        let peer_addr = downstream
            .get_socket_digest()
            .and_then(|digest| {
                digest
                    .peer_addr()
                    .and_then(|address| address.as_inet().copied())
            })
            .map(|address| address.ip());
        let mut session = self.runtime.session_with_peer_addr(peer_addr);
        let mut previous_snapshot = session.client_snapshot();
        let mut access_counters = RtmpAccessCounters::default();
        let mut last_failure_code = None;
        let mut buffer = [0; RTMP_READ_BUFFER_SIZE];
        let mut shutdown = shutdown.clone();
        let mut playback_drain = interval(RTMP_PLAYBACK_DRAIN_INTERVAL);
        playback_drain.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut control_poll = interval(RTMP_CONTROL_POLL_INTERVAL);
        control_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut publisher_liveness = interval(RTMP_PUBLISHER_LIVENESS_CHECK_INTERVAL);
        publisher_liveness.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let outbound = tokio::select! {
                _ = shutdown.changed() => break,
                _ = control_poll.tick() => {
                    if session.take_control_action().is_some() {
                        break;
                    }
                    Vec::new()
                }
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
                    access_counters.bytes_received = access_counters
                        .bytes_received
                        .saturating_add(bytes_received);
                    access_counters.messages_received = access_counters
                        .messages_received
                        .saturating_add(1);
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
                        Ok(outbound) => {
                            if let Some(code) = session.take_access_failure_code()
                                && let Some(snapshot) = session.client_snapshot()
                            {
                                log_rtmp_access_event(
                                    &self.service,
                                    &self.listener,
                                    rejection_event(code),
                                    RtmpAccessResult::Rejected,
                                    &snapshot,
                                    &access_counters,
                                    session_started_at,
                                    at_unix_ms,
                                    Some(rejection_failure_code(code)),
                                );
                            }
                            outbound
                        }
                        Err(error) => {
                            let failure_code = rtmp_session_failure_code(&error);
                            last_failure_code = Some(failure_code);
                            if let Some(snapshot) = session.client_snapshot()
                                && snapshot.connected
                            {
                                log_rtmp_access_event(
                                    &self.service,
                                    &self.listener,
                                    event_for_role(snapshot.role),
                                    RtmpAccessResult::Failed,
                                    &snapshot,
                                    &access_counters,
                                    session_started_at,
                                    at_unix_ms,
                                    Some(failure_code),
                                );
                            }
                            warn!("RTMP session failed: {error}");
                            break;
                        }
                    }
                }
            };
            if let Some(current_snapshot) = session.client_snapshot() {
                log_rtmp_state_transition(
                    &self.service,
                    &self.listener,
                    self.generation.registry(),
                    previous_snapshot.as_ref(),
                    &current_snapshot,
                    &access_counters,
                    session_started_at,
                    at_unix_ms,
                );
                previous_snapshot = Some(current_snapshot);
            }
            if outbound.is_empty() {
                continue;
            }
            if let Err(error) = write_rtmp_packets(
                &mut downstream,
                &outbound,
                &self.metrics,
                &mut access_counters,
            )
            .await
            {
                last_failure_code = Some("transport_write_failed");
                if let Some(snapshot) = session.client_snapshot()
                    && snapshot.connected
                {
                    log_rtmp_access_event(
                        &self.service,
                        &self.listener,
                        event_for_role(snapshot.role),
                        RtmpAccessResult::Failed,
                        &snapshot,
                        &access_counters,
                        session_started_at,
                        at_unix_ms,
                        last_failure_code,
                    );
                }
                warn!("RTMP transport write failed: {error}");
                break;
            }
        }

        let terminal_snapshot = session.client_snapshot();
        if let Some(snapshot) = terminal_snapshot.as_ref()
            && snapshot.connected
            && snapshot.role == RtmpSessionRole::Publisher
        {
            log_rtmp_auxiliary_failures(
                &self.service,
                &self.listener,
                self.generation.registry(),
                snapshot,
                &access_counters,
                session_started_at,
                at_unix_ms,
            );
        }
        let close_failed = if let Err(error) = session.close(at_unix_ms) {
            last_failure_code = Some("session_close_failed");
            warn!("could not detach RTMP media role: {error}");
            true
        } else {
            false
        };
        if let Some(snapshot) = terminal_snapshot {
            if snapshot.connected {
                log_rtmp_access_event(
                    &self.service,
                    &self.listener,
                    RtmpAccessEvent::Disconnect,
                    if close_failed {
                        RtmpAccessResult::Failed
                    } else {
                        RtmpAccessResult::Closed
                    },
                    &snapshot,
                    &access_counters,
                    session_started_at,
                    at_unix_ms,
                    last_failure_code,
                );
            } else {
                log_rtmp_access_event(
                    &self.service,
                    &self.listener,
                    RtmpAccessEvent::Connect,
                    RtmpAccessResult::Rejected,
                    &snapshot,
                    &access_counters,
                    session_started_at,
                    at_unix_ms,
                    Some(last_failure_code.unwrap_or("connect_rejected")),
                );
            }
        }
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn log_rtmp_state_transition(
    service: &RtmpServicePlan,
    listener: &str,
    registry: &RtmpRegistry,
    previous: Option<&RtmpClientSnapshot>,
    current: &RtmpClientSnapshot,
    counters: &RtmpAccessCounters,
    session_started_at: Instant,
    at_unix_ms: u64,
) {
    if !previous.is_some_and(|snapshot| snapshot.connected) && current.connected {
        log_rtmp_access_event(
            service,
            listener,
            RtmpAccessEvent::Connect,
            RtmpAccessResult::Accepted,
            current,
            counters,
            session_started_at,
            at_unix_ms,
            None,
        );
    }
    let previous_role = previous.map_or(RtmpSessionRole::Client, |snapshot| snapshot.role);
    if previous_role == current.role {
        return;
    }
    match current.role {
        RtmpSessionRole::Publisher => {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Publish,
                RtmpAccessResult::Accepted,
                current,
                counters,
                session_started_at,
                at_unix_ms,
                None,
            );
            let catalog = registry.snapshot();
            if let Some(stream) = catalog.streams.iter().find(|stream| {
                stream
                    .publisher
                    .is_some_and(|publisher| publisher.session_id == current.session_id)
            }) {
                if stream.recorders.iter().any(|recorder| {
                    matches!(
                        recorder.phase,
                        RecorderPhase::Starting { .. } | RecorderPhase::Recording { .. }
                    )
                }) {
                    log_rtmp_access_event(
                        service,
                        listener,
                        RtmpAccessEvent::Record,
                        RtmpAccessResult::Accepted,
                        current,
                        counters,
                        session_started_at,
                        at_unix_ms,
                        None,
                    );
                }
                if !stream.relays.is_empty() {
                    log_rtmp_access_event(
                        service,
                        listener,
                        RtmpAccessEvent::Relay,
                        RtmpAccessResult::Accepted,
                        current,
                        counters,
                        session_started_at,
                        at_unix_ms,
                        None,
                    );
                }
            }
        }
        RtmpSessionRole::Subscriber => {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Play,
                RtmpAccessResult::Accepted,
                current,
                counters,
                session_started_at,
                at_unix_ms,
                None,
            );
        }
        RtmpSessionRole::Client => {}
    }
}

fn log_rtmp_auxiliary_failures(
    service: &RtmpServicePlan,
    listener: &str,
    registry: &RtmpRegistry,
    snapshot: &RtmpClientSnapshot,
    counters: &RtmpAccessCounters,
    session_started_at: Instant,
    at_unix_ms: u64,
) {
    let catalog = registry.snapshot();
    let Some(stream) = catalog.streams.iter().find(|stream| {
        stream
            .publisher
            .is_some_and(|publisher| publisher.session_id == snapshot.session_id)
    }) else {
        return;
    };
    for recorder in &stream.recorders {
        if let RecorderPhase::Failed { code, .. } = recorder.phase {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Record,
                RtmpAccessResult::Failed,
                snapshot,
                counters,
                session_started_at,
                at_unix_ms,
                Some(recorder_failure_code(code)),
            );
        }
    }
    for relay in &stream.relays {
        if let Some(failure) = relay.status.last_failure {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Relay,
                RtmpAccessResult::Failed,
                snapshot,
                counters,
                session_started_at,
                at_unix_ms,
                Some(relay_failure_code(failure)),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn log_rtmp_access_event(
    service: &RtmpServicePlan,
    listener: &str,
    event: RtmpAccessEvent,
    result: RtmpAccessResult,
    snapshot: &RtmpClientSnapshot,
    counters: &RtmpAccessCounters,
    session_started_at: Instant,
    at_unix_ms: u64,
    failure_code: Option<&str>,
) {
    emit_rtmp_access(event.as_str(), result.as_str());
    let duration_ms = u64::try_from(session_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let value = serde_json::json!({
        "timestampUnixMs": at_unix_ms,
        "event": event.as_str(),
        "result": result.as_str(),
        "listener": listener,
        "service": service.service_id(),
        "application": snapshot.application.as_deref(),
        "stream": snapshot.stream_name.as_deref(),
        "sessionId": snapshot.session_id.to_string(),
        "role": snapshot.role.as_str(),
        "bytesReceived": counters.bytes_received,
        "bytesSent": counters.bytes_sent,
        "messagesReceived": counters.messages_received,
        "messagesSent": counters.messages_sent,
        "durationMs": duration_ms,
        "failureCode": failure_code,
    });
    if let Err(error) = service.write_rtmp_access_event(&value) {
        warn!("RTMP access log write failed: {error}");
    }
}

fn event_for_role(role: RtmpSessionRole) -> RtmpAccessEvent {
    match role {
        RtmpSessionRole::Client => RtmpAccessEvent::Connect,
        RtmpSessionRole::Publisher => RtmpAccessEvent::Publish,
        RtmpSessionRole::Subscriber => RtmpAccessEvent::Play,
    }
}

fn rejection_event(code: &str) -> RtmpAccessEvent {
    match code {
        "NetStream.Publish.BadName" => RtmpAccessEvent::Publish,
        "NetStream.Play.Failed" | "NetStream.Play.StreamNotFound" => RtmpAccessEvent::Play,
        _ => RtmpAccessEvent::Connect,
    }
}

fn rejection_failure_code(code: &str) -> &'static str {
    match code {
        "NetConnection.Connect.Rejected" => "connect_rejected",
        "NetStream.Publish.BadName" => "publish_rejected",
        "NetStream.Play.StreamNotFound" => "play_stream_not_found",
        "NetStream.Play.Failed" => "play_rejected",
        _ => "request_rejected",
    }
}

fn rtmp_session_failure_code(error: &RtmpSessionError) -> &'static str {
    match error {
        RtmpSessionError::Handshake(_) => "handshake_failed",
        RtmpSessionError::Session(_) => "protocol_failed",
        RtmpSessionError::Catalog(_) => "catalog_failed",
        RtmpSessionError::LiveHub(_) => "fanout_failed",
        RtmpSessionError::MediaEvent(_) => "media_failed",
        RtmpSessionError::InboundChunkTooLarge(_) => "inbound_chunk_too_large",
        RtmpSessionError::MediaSequenceExhausted => "media_sequence_exhausted",
        RtmpSessionError::MissingMetadata => "missing_metadata",
        RtmpSessionError::NoActivePlayback => "playback_unavailable",
        RtmpSessionError::Callback(_) => "callback_failed",
        RtmpSessionError::Vod(_) => "vod_failed",
        RtmpSessionError::MessageStream { .. } => "message_stream_failed",
    }
}

fn recorder_failure_code(code: RecorderErrorCode) -> &'static str {
    match code {
        RecorderErrorCode::OpenFailed => "record_open_failed",
        RecorderErrorCode::WriteFailed => "record_write_failed",
        RecorderErrorCode::CloseFailed => "record_close_failed",
        RecorderErrorCode::BackendUnavailable => "record_backend_unavailable",
        RecorderErrorCode::FileSyncFailed => "record_file_sync_failed",
        RecorderErrorCode::PublishFailed => "record_publish_failed",
        RecorderErrorCode::DirectorySyncFailed => "record_directory_sync_failed",
        RecorderErrorCode::QueueDiscontinuity => "record_queue_discontinuity",
        RecorderErrorCode::UnsupportedCodec => "record_unsupported_codec",
        RecorderErrorCode::ShutdownTimedOut => "record_shutdown_timeout",
        RecorderErrorCode::WorkerPanicked => "record_worker_panicked",
        RecorderErrorCode::StalePublisher => "record_stale_publisher",
    }
}

fn relay_failure_code(failure: RtmpRelayFailure) -> &'static str {
    match failure {
        RtmpRelayFailure::Policy => "relay_policy",
        RtmpRelayFailure::Connect => "relay_connect_failed",
        RtmpRelayFailure::Handshake => "relay_handshake_failed",
        RtmpRelayFailure::Session => "relay_session_failed",
        RtmpRelayFailure::Transport => "relay_transport_failed",
        RtmpRelayFailure::Source => "relay_source_failed",
        RtmpRelayFailure::Thread => "relay_worker_unavailable",
    }
}

async fn write_rtmp_packets(
    downstream: &mut Stream,
    packets: &[Vec<u8>],
    metrics: &ListenerMetrics,
    counters: &mut RtmpAccessCounters,
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
                counters.bytes_sent = counters.bytes_sent.saturating_add(bytes_sent);
                remaining = &remaining[bytes_written..];
            }
            counters.messages_sent = counters.messages_sent.saturating_add(1);
        }
        downstream.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "RTMP write deadline exceeded"))?
}

#[allow(clippy::too_many_arguments)]
fn build_management_api(
    registry: Arc<RtmpRegistry>,
    vod_catalog: Arc<VodCatalog>,
    media_catalog: Arc<MediaCatalog>,
    metrics: RuntimeMetrics,
    topology: Arc<TopologySnapshot>,
    coordinator: CanonicalConfigCoordinator,
    active_revision: EffectiveRevision,
    token_file: &Path,
    ui_dir: Option<&Path>,
    mode: oxiroute_server::RuntimeMode,
) -> io::Result<RtmpManagementApi> {
    let api = if let Some(ui_dir) = ui_dir {
        RtmpManagementApi::with_ui_dir(registry, metrics, topology, ui_dir)
    } else {
        Ok(RtmpManagementApi::new(registry, metrics, topology))
    }?;
    api.with_vod_catalog(vod_catalog)
        .with_media_catalog(media_catalog)
        .with_config_coordinator_from_token_file(coordinator, active_revision, token_file, mode)
}

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    if let Some(exit_code) = supervised::dispatch() {
        return exit_code;
    }

    env_logger::init();

    let cli = Cli::parse_process();
    let result = match cli.command() {
        Command::Serve { config } => {
            #[cfg(target_os = "linux")]
            {
                supervised::master::run_if_supported(config)
            }
            #[cfg(not(target_os = "linux"))]
            {
                run(config)
            }
        }
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
    retirement: CandidateRtmpRetirementHandle,
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: JoinHandle<()>,
}

#[derive(Clone)]
struct CandidateRtmpRetirementHandle {
    lifecycles: Arc<Mutex<Option<Vec<RtmpRecorderLifecycle>>>>,
}

impl CandidateRtmpRetirementHandle {
    fn capture(generation: &RuntimeGeneration) -> Self {
        let mut lifecycles = Vec::new();
        for runtime in generation.services() {
            let ServiceKind::Rtmp(plan) = &runtime.kind else {
                continue;
            };
            let Some(runtime) = generation.rtmp_runtime(plan.service_id()) else {
                continue;
            };
            let Some(lifecycle) = runtime.recorder_lifecycle() else {
                continue;
            };
            if !lifecycles
                .iter()
                .any(|existing: &RtmpRecorderLifecycle| existing.is_same_lifecycle(&lifecycle))
            {
                lifecycles.push(lifecycle);
            }
        }
        Self {
            lifecycles: Arc::new(Mutex::new(Some(lifecycles))),
        }
    }

    fn initiate(&self, deadline: std::time::Instant) -> Vec<RtmpRecorderShutdown> {
        self.lifecycles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_default()
            .into_iter()
            .map(|lifecycle| lifecycle.initiate_shutdown(deadline))
            .collect()
    }
}

const ACME_RENEWAL_SCAN_INTERVAL: Duration = Duration::from_hours(12);
const ACME_RENEWAL_OPERATION_TIMEOUT: Duration = Duration::from_mins(10);
const GENERATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct AcmeManagedSupervisor {
    stop: Option<mpsc::Sender<()>>,
    cancellation: Dns01Cancellation,
    thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    probe: Option<Arc<AcmeSupervisorProbe>>,
}

impl AcmeManagedSupervisor {
    fn start(reconcilers: Vec<Arc<AcmeManagedReconciler>>) -> io::Result<Self> {
        Self::start_internal(reconcilers, None)
    }

    fn start_internal(
        reconcilers: Vec<Arc<AcmeManagedReconciler>>,
        #[cfg(test)] probe: Option<Arc<AcmeSupervisorProbe>>,
        #[cfg(not(test))] _probe: Option<()>,
    ) -> io::Result<Self> {
        let (stop, stop_rx) = mpsc::channel();
        let cancellation = Dns01Cancellation::new();
        let worker_cancellation = cancellation.clone();
        #[cfg(test)]
        let worker_probe = probe.clone();
        let thread = thread::Builder::new()
            .name("oxiroute-acme-renewal".into())
            .spawn(move || {
                #[cfg(test)]
                let _retained = worker_probe.as_ref().and_then(|probe| {
                    probe
                        .retained
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                });
                #[cfg(test)]
                if worker_probe
                    .as_ref()
                    .is_some_and(|probe| probe.block_worker)
                {
                    if let Some(probe) = &worker_probe {
                        probe.iterations.fetch_add(1, Ordering::Release);
                    }
                    let _ = worker_cancellation.wait_timeout(Duration::from_secs(1));
                    if let Some(probe) = &worker_probe {
                        probe.worker_cleanups.fetch_add(1, Ordering::Release);
                    }
                    return;
                }
                loop {
                    #[cfg(test)]
                    if let Some(probe) = &worker_probe {
                        probe.iterations.fetch_add(1, Ordering::Relaxed);
                    }
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
                        let operation = AcmeOperation::with_deadline(
                            worker_cancellation.clone(),
                            std::time::Instant::now() + ACME_RENEWAL_OPERATION_TIMEOUT,
                        );
                        match reconciler.renew_now_with_operation(operation) {
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
                    match stop_rx.recv_timeout(acme_scan_delay(&reconcilers, now)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            cancellation,
            thread: Some(thread),
            #[cfg(test)]
            probe,
        })
    }

    fn shutdown(&mut self) {
        self.cancellation.cancel();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
            #[cfg(test)]
            if let Some(probe) = &self.probe {
                probe.stops.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
            #[cfg(test)]
            if let Some(probe) = &self.probe {
                probe.joins.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
struct AcmeSupervisorProbe {
    iterations: AtomicU64,
    stops: AtomicU64,
    joins: AtomicU64,
    retained: Mutex<Option<Arc<()>>>,
    block_worker: bool,
    worker_cleanups: AtomicU64,
}

trait GenerationRuntimeJoin {
    fn join_runtime(self) -> io::Result<()>;
}

impl GenerationRuntimeJoin for Http3Runtime {
    fn join_runtime(self) -> io::Result<()> {
        self.join()
    }
}

impl GenerationRuntimeJoin for UdpRuntime {
    fn join_runtime(self) -> io::Result<()> {
        self.join()
    }
}

fn join_generation_runtimes<H, U>(
    _acme_supervisor: AcmeManagedSupervisor,
    http3_runtimes: Vec<H>,
    udp_runtimes: Vec<U>,
) -> io::Result<()>
where
    H: GenerationRuntimeJoin,
    U: GenerationRuntimeJoin,
{
    for runtime in http3_runtimes {
        runtime.join_runtime()?;
    }
    for runtime in udp_runtimes {
        runtime.join_runtime()?;
    }
    Ok(())
}

impl Drop for AcmeManagedSupervisor {
    fn drop(&mut self) {
        self.shutdown();
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
        manager: &GenerationManager,
        process_shutdown: &Arc<AtomicBool>,
        deadline: std::time::Instant,
    ) -> Result<Self, Box<dyn Error>> {
        let retirement = CandidateRtmpRetirementHandle::capture(&generation);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (setup_tx, setup_rx) = mpsc::sync_channel(1);
        let thread_generation = Arc::clone(&generation);
        let thread_process_shutdown = Arc::clone(process_shutdown);
        let thread_manager = manager.clone();
        let thread = match thread::Builder::new()
            .name(format!(
                "oxiroute-generation-{}",
                &generation.revision().candidate.as_str()[..12]
            ))
            .spawn(move || {
                if let Err(error) = serve_generation(
                    &thread_generation,
                    &coordinator,
                    &thread_manager,
                    thread_process_shutdown,
                    shutdown_rx,
                    &setup_tx,
                ) {
                    let _ = setup_tx.try_send(Err(error.to_string()));
                    error!("generation runtime failed: {error}");
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                finish_candidate_generation(manager, &generation, deadline);
                return Err(error.into());
            }
        };
        loop {
            if process_shutdown.load(Ordering::Acquire) {
                let process = Self {
                    generation: Arc::clone(&generation),
                    retirement,
                    shutdown: shutdown_tx,
                    thread,
                };
                let _ = finish_candidate_generation_process(manager, process, deadline);
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
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    });
                }
                Ok(Err(error)) => {
                    let process = Self {
                        generation: Arc::clone(&generation),
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    };
                    let _ = finish_candidate_generation_process(manager, process, deadline);
                    return Err(io::Error::other(error).into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let process = Self {
                        generation: Arc::clone(&generation),
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    };
                    let _ = finish_candidate_generation_process(manager, process, deadline);
                    return Err(
                        io::Error::other("generation setup terminated without readiness").into(),
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) if std::time::Instant::now() >= deadline => {
                    let process = Self {
                        generation: Arc::clone(&generation),
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    };
                    let _ = finish_candidate_generation_process(manager, process, deadline);
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

fn finish_drained_generation_process(
    manager: &GenerationManager,
    process: GenerationProcess,
    deadline: std::time::Instant,
) -> bool {
    let retirement = process.retirement.clone();
    let recorder_shutdowns = retirement.initiate(deadline);
    let mut clean = finish_generation_process(process, deadline);
    for shutdown in recorder_shutdowns {
        clean &= shutdown.wait_until(deadline);
    }
    manager.prune_completed();
    clean
}

fn finish_candidate_generation_process(
    manager: &GenerationManager,
    process: GenerationProcess,
    deadline: std::time::Instant,
) -> bool {
    if manager
        .active()
        .is_some_and(|active| Arc::ptr_eq(&active, &process.generation))
    {
        return shutdown_generation_processes(manager, vec![process], deadline);
    }
    finish_drained_generation_process(manager, process, deadline)
}

fn finish_candidate_generation(
    manager: &GenerationManager,
    generation: &Arc<RuntimeGeneration>,
    deadline: std::time::Instant,
) {
    if manager
        .active()
        .is_some_and(|active| Arc::ptr_eq(&active, generation))
    {
        return;
    }
    let recorder_shutdowns = generation.initiate_recorder_shutdown(deadline);
    for shutdown in &recorder_shutdowns {
        let _ = shutdown.wait_until(deadline);
    }
    manager.prune_completed();
}

fn shutdown_generation_processes(
    manager: &GenerationManager,
    processes: Vec<GenerationProcess>,
    deadline: std::time::Instant,
) -> bool {
    let recorder_shutdowns = manager.begin_shutdown(deadline);
    for process in &processes {
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
    let mut clean = true;
    for shutdown in &recorder_shutdowns {
        clean &= shutdown.wait_until(deadline);
    }
    for process in processes {
        clean &= finish_generation_process(process, deadline);
    }
    manager.prune_completed();
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
                let drained = process.generation.drained();
                if drained {
                    process.request_shutdown();
                }
                if process.is_finished() {
                    let deadline = std::time::Instant::now() + GENERATION_SHUTDOWN_TIMEOUT;
                    successful &= if drained {
                        finish_drained_generation_process(manager, process, deadline)
                    } else {
                        finish_generation_process(process, deadline)
                    };
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
        let startup_deadline = std::time::Instant::now() + GENERATION_STARTUP_TIMEOUT;
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
            manager,
            process_shutdown,
            startup_deadline,
        ) {
            Ok(process) => {
                match wait_for_generation_ready(candidate.generation(), stop, startup_deadline) {
                    Ok(()) => match startup.activate() {
                        Ok(_) => {
                            retired.push(current);
                            current = process;
                        }
                        Err(error) => {
                            error!("candidate generation publication failed: {error}");
                            successful &= finish_candidate_generation_process(
                                manager,
                                process,
                                startup_deadline,
                            );
                        }
                    },
                    Err(error) => {
                        error!("candidate generation could not become ready: {error}");
                        successful &=
                            finish_candidate_generation_process(manager, process, startup_deadline);
                        manager.quarantine(&candidate, "runtime_start");
                    }
                }
            }
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
    deadline: std::time::Instant,
) -> Result<(), Box<dyn Error>> {
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
        let listeners = generation.metrics().internal_listener_snapshots()?;
        if generation.runtime_started()
            && listeners.len() == generation.expected_runtime_listener_count()
            && listeners
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
        oxiroute_server::RuntimeMode,
    ) -> notify::Result<ConfigWatcher>,
{
    match start(
        coordinator,
        manager.clone(),
        ConfigWatcherOptions::default(),
        oxiroute_server::RuntimeMode::Direct,
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
    let reload_requested = Arc::new(AtomicBool::new(false));
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;
    let signal_handle = signals.handle();
    let signal_stop = Arc::clone(&stop_supervisor);
    let signal_reload = Arc::clone(&reload_requested);
    let signal_thread = thread::Builder::new()
        .name("oxiroute-signals".into())
        .spawn(move || {
            for signal in signals.forever() {
                match signal {
                    SIGTERM | SIGINT => {
                        signal_stop.store(true, Ordering::Release);
                        break;
                    }
                    SIGHUP => signal_reload.store(true, Ordering::Release),
                    _ => {}
                }
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
    let startup_deadline = std::time::Instant::now() + GENERATION_STARTUP_TIMEOUT;
    let candidate = generation_manager.prepare_with_deadline(*config_document, startup_deadline)?;
    let mut startup = generation_manager.begin_candidate_start(&candidate)?;
    let starting_generation = startup.claim_runtime_start()?;
    let initial = GenerationProcess::start(
        starting_generation,
        config_coordinator.clone(),
        &generation_manager,
        &stop_supervisor,
        startup_deadline,
    )?;
    if let Err(error) =
        wait_for_generation_ready(candidate.generation(), &stop_supervisor, startup_deadline)
    {
        let _ = finish_candidate_generation_process(&generation_manager, initial, startup_deadline);
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
                let _ = finish_candidate_generation_process(&generation_manager, initial, deadline);
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
            let _ = finish_candidate_generation_process(&generation_manager, initial, deadline);
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
        if reload_requested.swap(false, Ordering::AcqRel) {
            config_watcher.wake();
        }
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
            let _ = shutdown.wait_until(deadline);
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
    let config = generation.config().as_draft();
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
    let services = generation.services().to_vec();
    let health_supervisor = generation.health_supervisor();
    let pools = generation.pools().to_vec();
    let tls = generation.tls();
    let challenge_store = tls.challenge_store().clone();
    let topology = Arc::clone(&plan.topology);
    let acme_reconcilers = tls.acme_reconcilers().to_vec();
    let certbot_reconcilers = tls.certbot_reconcilers().to_vec();
    let direct_file_reconcilers = tls.file_reconcilers().to_vec();
    let mut http3_runtimes = Vec::new();
    let mut udp_runtimes = Vec::new();
    if let Some(supervisor) = health_supervisor {
        server.add_service(background_service(
            "upstream health",
            GenerationBackgroundService {
                _generation: Arc::clone(generation),
                inner: supervisor,
            },
        ));
    }
    let runtime_metrics = generation.metrics().clone();
    let mut monitored_services = Vec::with_capacity(services.len());
    for spec in services {
        let metrics = generation.traffic_listener_metrics(&spec.name)?;
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
        let metrics = generation.management_listener_metrics()?;
        let management_api = build_management_api(
            Arc::clone(&rtmp_registry),
            Arc::clone(generation.rtmp_vod_catalog()),
            Arc::clone(generation.rtmp_media_catalog()),
            runtime_metrics.clone(),
            Arc::clone(&topology),
            config_coordinator.clone(),
            active_revision.clone(),
            management_token_file
                .as_deref()
                .expect("management token path was required above"),
            management.ui_dir.as_deref(),
            runtime_metrics.supervision_mode(),
        )?
        .with_generation_manager(generation_manager.clone())
        .with_process_shutdown(process_shutdown);
        let app = ProcessAdmissionApp::new(
            HttpListenerApp::new(management_api.into_http_app(), None)
                .with_generation(Arc::clone(generation)),
            metrics.clone(),
            Arc::clone(generation),
        );
        let mut service = Service::new("OxiRoute management".into(), app);
        service.add_tcp(&management.bind.to_string());
        server.add_service(
            RuntimeListenerService::new(
                service,
                management_reservation.expect("management listener reservation was prepared above"),
                Some(metrics),
            )
            .with_generation(Arc::clone(generation)),
        );
        info!("configured management API on {}", management.bind);
    }
    if let Some(stats) = &config.stats {
        for (index, bind) in stats.binds.iter().enumerate() {
            let metrics = generation.stats_listener_metrics(index)?;
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
                metrics.clone(),
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
                RuntimeListenerService::new(service, reservation, Some(metrics))
                    .with_generation(Arc::clone(generation)),
            );
            info!("configured HAProxy-compatible statistics on {bind}");
        }
        for (index, page) in stats.pages.iter().enumerate() {
            let app = HaproxyStatsPage::new(page, pools.clone(), generation_manager.clone());
            let metrics = generation.stats_page_listener_metrics(index)?;
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
        let listener_proxy_protocol = spec.proxy_protocol;

        match spec.kind {
            ServiceKind::ForwardHttp1(forward_service) => {
                let mut service = Service::new(
                    service_name,
                    ForwardHttp1RuntimeApp::new(
                        ForwardHttp1App::new(
                            forward_service,
                            metrics.clone(),
                            downstream_timeouts
                                .request_timeout_ms
                                .map(Duration::from_millis),
                            listener_tls.is_some(),
                            challenge_store.clone(),
                        ),
                        metrics.clone(),
                        Arc::clone(generation),
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
                    )
                    .with_generation(Arc::clone(generation)),
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
                    RtmpIngest::new(
                        listener_name.clone(),
                        runtime,
                        rtmp_service,
                        metrics.clone(),
                        Arc::clone(generation),
                    ),
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
                    TcpRelay::new(l4_service, metrics.clone(), listener_proxy_protocol)
                        .with_generation(Arc::clone(generation)),
                );
                add_plain_listener(&mut service, &listener_name, &listener_bind)?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::ForwardHttp3(forward_service) => {
                let Some(listener_tls) = listener_tls else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("HTTP/3 listener `{listener_name}` requires a TLS profile"),
                    )
                    .into());
                };
                let runtime = Http3Runtime::start_forward(
                    listener_name.clone(),
                    reservation,
                    forward_service,
                    listener_tls,
                    Arc::clone(generation),
                    metrics,
                    shutdown.clone(),
                )
                .inspect_err(|_| {
                    generation.mark_runtime_failed();
                })?;
                http3_runtimes.push(runtime);
            }
            ServiceKind::Http3(http_service) => {
                let Some(listener_tls) = listener_tls else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("HTTP/3 listener `{listener_name}` requires a TLS profile"),
                    )
                    .into());
                };
                let runtime = Http3Runtime::start_reverse(
                    listener_name.clone(),
                    reservation,
                    http_service,
                    listener_tls,
                    Arc::clone(generation),
                    metrics,
                    downstream_timeouts,
                    shutdown.clone(),
                )
                .inspect_err(|_| {
                    generation.mark_runtime_failed();
                })?;
                http3_runtimes.push(runtime);
            }
            ServiceKind::Udp(l4_service) => {
                let runtime = UdpRuntime::start(
                    listener_name.clone(),
                    reservation,
                    l4_service,
                    Arc::clone(generation),
                    metrics,
                    listener_proxy_protocol,
                    shutdown.clone(),
                )
                .inspect_err(|_| {
                    generation.mark_runtime_failed();
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
    join_generation_runtimes(acme_supervisor, http3_runtimes, udp_runtimes)?;
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
        ConfigDraft, HttpVersionPolicy, L4Service, Listener, ListenerBind, Protocol,
        RtmpApplication, RtmpRecorder, RtmpRecorderStart, RtmpService, UpstreamAlgorithm,
        UpstreamEndpoint, UpstreamPool,
    };
    use oxiroute_rtmp::RtmpSession;
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

    struct InjectedRuntimeJoin {
        result: io::Result<()>,
    }

    impl GenerationRuntimeJoin for InjectedRuntimeJoin {
        fn join_runtime(self) -> io::Result<()> {
            self.result
        }
    }

    #[tokio::test]
    async fn generation_runtime_marker_publishes_readiness_after_start_claim() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("oxiroute.kdl");
        let config = ConfigDraft {
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
        fs::write(
            &path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Kdl,
                &config.validate().expect("valid config"),
            )
            .expect("render"),
        )
        .expect("config");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("load");
        };
        let manager = GenerationManager::new();
        let candidate = manager.prepare(*document).expect("candidate");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        let marker = GenerationRuntimeMarker {
            generation: Arc::clone(&generation),
        };
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (ready_tx, mut ready) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut marker = marker;
            marker
                .start_service(None, shutdown, 1, ServiceReadyNotifier::new(ready_tx))
                .await;
        });

        ready.changed().await.expect("generation readiness");
        assert!(*ready.borrow());
        assert!(generation.runtime_started());

        shutdown_tx.send(true).expect("generation shutdown");
        task.await.expect("generation marker task");
        drop(startup);
    }

    fn acme_supervisor_probe(retained: Arc<()>) -> Arc<AcmeSupervisorProbe> {
        Arc::new(AcmeSupervisorProbe {
            iterations: AtomicU64::new(0),
            stops: AtomicU64::new(0),
            joins: AtomicU64::new(0),
            retained: Mutex::new(Some(retained)),
            block_worker: false,
            worker_cleanups: AtomicU64::new(0),
        })
    }

    fn wait_for_acme_iteration(probe: &AcmeSupervisorProbe) {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while probe.iterations.load(Ordering::Acquire) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "ACME worker did not start"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn acme_supervisor_shutdown_is_idempotent_and_releases_worker_authority() {
        let retained = Arc::new(());
        let weak = Arc::downgrade(&retained);
        let probe = acme_supervisor_probe(retained);
        let mut supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
            .expect("ACME supervisor");
        wait_for_acme_iteration(&probe);

        supervisor.shutdown();
        supervisor.shutdown();
        drop(supervisor);

        assert_eq!(probe.stops.load(Ordering::Acquire), 1);
        assert_eq!(probe.joins.load(Ordering::Acquire), 1);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn acme_supervisor_cancels_controlled_wait_and_cleans_worker_once() {
        let retained = Arc::new(());
        let weak = Arc::downgrade(&retained);
        let probe = Arc::new(AcmeSupervisorProbe {
            iterations: AtomicU64::new(0),
            stops: AtomicU64::new(0),
            joins: AtomicU64::new(0),
            retained: Mutex::new(Some(retained)),
            block_worker: true,
            worker_cleanups: AtomicU64::new(0),
        });
        let mut supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
            .expect("ACME supervisor");
        wait_for_acme_iteration(&probe);
        let started = std::time::Instant::now();

        supervisor.shutdown();
        supervisor.shutdown();

        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(probe.stops.load(Ordering::Acquire), 1);
        assert_eq!(probe.joins.load(Ordering::Acquire), 1);
        assert_eq!(probe.worker_cleanups.load(Ordering::Acquire), 1);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn disconnected_acme_stop_channel_terminates_without_spinning() {
        let retained = Arc::new(());
        let weak = Arc::downgrade(&retained);
        let probe = acme_supervisor_probe(retained);
        let mut supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
            .expect("ACME supervisor");
        wait_for_acme_iteration(&probe);

        drop(supervisor.stop.take());
        let thread = supervisor.thread.take().expect("ACME worker");
        thread.join().expect("disconnected worker exits");
        let iterations = probe.iterations.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(10));

        assert_eq!(probe.iterations.load(Ordering::Acquire), iterations);
        assert_eq!(iterations, 1);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn later_runtime_join_failures_drop_and_join_acme_supervisor_once() {
        for (http3, udp, expected) in [
            (
                vec![InjectedRuntimeJoin {
                    result: Err(io::Error::other("injected H3 join failure")),
                }],
                vec![InjectedRuntimeJoin { result: Ok(()) }],
                "injected H3 join failure",
            ),
            (
                vec![InjectedRuntimeJoin { result: Ok(()) }],
                vec![InjectedRuntimeJoin {
                    result: Err(io::Error::other("injected UDP join failure")),
                }],
                "injected UDP join failure",
            ),
        ] {
            let retained = Arc::new(());
            let weak = Arc::downgrade(&retained);
            let probe = acme_supervisor_probe(retained);
            let supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
                .expect("ACME supervisor");
            wait_for_acme_iteration(&probe);

            let error = join_generation_runtimes(supervisor, http3, udp)
                .expect_err("injected join failure");

            assert_eq!(error.to_string(), expected);
            assert_eq!(probe.stops.load(Ordering::Acquire), 1);
            assert_eq!(probe.joins.load(Ordering::Acquire), 1);
            assert!(weak.upgrade().is_none());
        }
    }

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
        publish_stalled_recorder(&generation, "live");
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let thread = thread::spawn(|| {});
        while !thread.is_finished() {
            thread::yield_now();
        }
        let process = GenerationProcess {
            retirement: CandidateRtmpRetirementHandle::capture(&generation),
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
            retirement: CandidateRtmpRetirementHandle::capture(&generation),
            generation: Arc::clone(&generation),
            shutdown,
            thread,
        })));
        let started = std::time::Instant::now();

        let result =
            start_config_watcher_or_shutdown(coordinator, &manager, &initial, move |_, _, _, _| {
                publish_stalled_recorder(&generation, "live");
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
        let config = ConfigDraft {
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
            retirement: CandidateRtmpRetirementHandle::capture(&generation),
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
            retirement: CandidateRtmpRetirementHandle {
                lifecycles: Arc::new(Mutex::new(Some(Vec::new()))),
            },
            shutdown,
            thread,
        };
        assert_eq!(
            completed.join_until(std::time::Instant::now()),
            GenerationJoinOutcome::Panicked
        );
    }

    #[test]
    fn candidate_failure_cleanup_detaches_at_its_absolute_deadline() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("oxiroute.kdl");
        let recording_root = directory.path().join("recordings");
        fs::create_dir(&recording_root).expect("recording root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener address");
        let listener_address = listener.local_addr().expect("listener address");
        drop(listener);
        let config = recorder_runtime_config(listener_address, &recording_root);
        fs::write(
            &path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Kdl,
                &config.clone().validate().expect("valid config"),
            )
            .expect("render"),
        )
        .expect("config");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("load")
        };
        let manager = GenerationManager::new();
        let candidate = manager.prepare(*document).expect("candidate");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        let detached_runtime = generation
            .rtmp_runtime("live")
            .expect("RTMP runtime")
            .clone();
        let (release, released) = mpsc::sync_channel(0);
        let (finished, completion) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            released.recv().expect("release candidate process");
            assert!(detached_runtime.session().receive(&[], 0).is_ok());
            finished.send(()).expect("candidate completion receiver");
        });
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let process = GenerationProcess {
            retirement: CandidateRtmpRetirementHandle::capture(&generation),
            generation,
            shutdown,
            thread,
        };
        let started = std::time::Instant::now();

        assert!(finish_candidate_generation_process(
            &manager,
            process,
            started + Duration::from_millis(75),
        ));
        let elapsed = started.elapsed();

        assert!(elapsed >= Duration::from_millis(50));
        assert!(elapsed < Duration::from_millis(250));
        let retry = oxiroute_rtmp::RecordingStore::open(
            &recording_root,
            oxiroute_rtmp::RecordingStoreLimits {
                max_bytes: None,
                max_files: None,
                max_active_recorders: usize::try_from(
                    config.rtmp_services[0].applications[0].recorders[0].max_active_recorders,
                )
                .expect("validated recorder limit"),
            },
        )
        .expect("detached candidate released recording root");
        drop(retry);
        release.send(()).expect("release detached candidate");
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("detached candidate exit");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn candidate_cleanup_visits_later_services_after_the_first_shutdown_times_out() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("oxiroute.kdl");
        let first_root = directory.path().join("first-recordings");
        let second_root = directory.path().join("second-recordings");
        fs::create_dir(&first_root).expect("first recording root");
        fs::create_dir(&second_root).expect("second recording root");
        let first_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("first listener");
        let first_address = first_listener.local_addr().expect("first address");
        drop(first_listener);
        let second_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("second listener");
        let second_address = second_listener.local_addr().expect("second address");
        drop(second_listener);
        let mut config = recorder_runtime_config(first_address, &first_root);
        let mut second_service = config.rtmp_services[0].clone();
        second_service.name = "second".into();
        second_service.applications[0].recorders[0].root_directory = second_root.clone();
        config.rtmp_services.push(second_service);
        let mut second_listener = config.listeners[0].clone();
        second_listener.name = "second".into();
        second_listener.bind = ListenerBind::Socket {
            address: second_address,
        };
        second_listener.service = Some("second".into());
        config.listeners.push(second_listener);
        fs::write(
            &path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Kdl,
                &config.clone().validate().expect("valid config"),
            )
            .expect("render"),
        )
        .expect("config");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("load")
        };
        let manager = GenerationManager::new();
        let candidate = manager.prepare(*document).expect("candidate");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        let ownership = fs::File::open(&first_root).expect("first root ownership");
        flock(&ownership, FlockOperation::LockExclusive).expect("stall first recorder");
        publish_stalled_recorder(&generation, "live");
        let (release, released) = mpsc::sync_channel(0);
        let thread = thread::spawn(move || released.recv().expect("release candidate process"));
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let process = GenerationProcess {
            retirement: CandidateRtmpRetirementHandle::capture(&generation),
            generation,
            shutdown,
            thread,
        };
        let deadline = std::time::Instant::now() + Duration::from_millis(75);

        assert!(finish_candidate_generation_process(
            &manager, process, deadline
        ));

        let retry_path = directory.path().join("retry.kdl");
        let mut retry_config = config.clone();
        retry_config.listeners[0].bind = ListenerBind::Socket {
            address: std::net::TcpListener::bind("127.0.0.1:0")
                .expect("retry listener")
                .local_addr()
                .expect("retry listener address"),
        };
        retry_config.listeners[1].bind = ListenerBind::Socket {
            address: std::net::TcpListener::bind("127.0.0.1:0")
                .expect("retry second listener")
                .local_addr()
                .expect("retry second listener address"),
        };
        fs::write(
            &retry_path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Kdl,
                &retry_config.validate().expect("retry config"),
            )
            .expect("render retry config"),
        )
        .expect("write retry config");
        let retry_coordinator =
            CanonicalConfigCoordinator::new(&retry_path).expect("retry coordinator");
        let ConfigLoadOutcome::Loaded(retry_document) = retry_coordinator.load() else {
            panic!("retry load")
        };
        let retry_manager = GenerationManager::new();
        let retry_status = retry_manager.clone();
        let retry_deadline = std::time::Instant::now() + Duration::from_millis(50);
        let (first_retry_tx, first_retry_rx) = mpsc::sync_channel(1);
        let first_retry_thread = thread::spawn(move || {
            first_retry_tx
                .send(retry_manager.prepare_with_deadline(*retry_document, retry_deadline))
                .expect("first retired service recording retry result");
        });
        match first_retry_rx.recv_timeout(Duration::from_millis(100)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Err(error)) => panic!("retry finished early: {}", error.code()),
            Ok(Ok(_)) => panic!("retry unexpectedly succeeded"),
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("retry disconnected"),
        }
        assert!(retry_status.status().quarantined_revision.is_none());

        let limits = oxiroute_rtmp::RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(32),
            max_active_recorders: 4,
        };
        drop(
            oxiroute_rtmp::RecordingStore::open(&second_root, limits)
                .expect("later retired service recording retry"),
        );
        flock(&ownership, FlockOperation::Unlock).expect("release first recording root");
        assert!(matches!(
            first_retry_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first retired service recording retry after lock release"),
            Err(oxiroute_server::GenerationError::PreparationTimedOut)
        ));
        assert!(retry_status.status().quarantined_revision.is_none());
        first_retry_thread.join().expect("first recording retry");
        release.send(()).expect("release detached candidate");
        let ConfigLoadOutcome::Loaded(retry_document) = retry_coordinator.load() else {
            panic!("retry config after release")
        };
        assert!(
            retry_status
                .prepare_with_deadline(
                    *retry_document,
                    std::time::Instant::now() + Duration::from_secs(1),
                )
                .is_ok()
        );
        let reopen_deadline = std::time::Instant::now() + Duration::from_secs(1);
        drop(
            oxiroute_rtmp::RecordingStore::open_with_deadline(
                &first_root,
                limits,
                Some(reopen_deadline),
            )
            .expect("first retired service recording retry after worker exit"),
        );
    }

    fn activate_test_generation(
        path: &Path,
        config: &ConfigDraft,
    ) -> (
        CanonicalConfigCoordinator,
        GenerationManager,
        Arc<RuntimeGeneration>,
    ) {
        fs::write(
            path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Kdl,
                &config.clone().validate().expect("valid test config"),
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

    fn publish_stalled_recorder(generation: &RuntimeGeneration, service: &str) {
        let mut server = generation
            .rtmp_runtime(service)
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
    ) -> ConfigDraft {
        ConfigDraft {
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
                proxy_protocol: None,
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
                max_inbound_message_size: 8 * 1024 * 1024,
                ack_window_size: 5_000_000,
                access_log: None,
                outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
                callbacks: oxiroute_config::RtmpCallbackConfig::default(),
                auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
                exec_profiles: Vec::new(),
                applications: vec![RtmpApplication {
                    name: "live".into(),
                    live: true,
                    idle_streams: true,
                    publish: oxiroute_config::RtmpAccessPolicy::default(),
                    play: oxiroute_config::RtmpAccessPolicy::default(),
                    limits: oxiroute_config::RtmpSessionCeilings::default(),
                    push_targets: Vec::new(),
                    pull_targets: Vec::new(),
                    relay: oxiroute_config::RtmpRelayPolicy::default(),
                    callbacks: oxiroute_config::RtmpCallbackConfig::default(),
                    fanout: oxiroute_config::RtmpFanoutPolicy::default(),
                    vod: None,
                    hls: None,
                    dash: None,
                    recorders: vec![RtmpRecorder {
                        name: "archive".into(),
                        start: RtmpRecorderStart::Continuous,
                        root_directory: recording_root.to_path_buf(),
                        record_mask: oxiroute_config::RtmpRecordMask::default(),
                        suffix_template: ".flv".into(),
                        append_unix_seconds: false,
                        append: false,
                        lock: false,
                        max_size: None,
                        max_frames: None,
                        notify: false,
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
        let config = ConfigDraft {
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
                proxy_protocol: None,
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
                health_check: None,
                passive_health: None,
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
                proxy_protocol: None,
                udp: None,
            }],
        };
        let config = config.validate().expect("valid TCP config");
        let mut acquired = oxiroute_server::service_specs(&config).expect("TCP runtime services");
        let spec = acquired.remove(0);
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
        let app = Arc::new(TcpRelay::new(service, listener_metrics, None));
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
        let mut counters = RtmpAccessCounters::default();

        write_rtmp_packets(&mut server, &packets, &metrics, &mut counters)
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
        assert_eq!(counters.bytes_sent, 12);
        assert_eq!(counters.messages_sent, 2);
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
            runtime
                .internal_listener_snapshots()
                .expect("active snapshot")[0]
                .state,
            oxiroute_server::ListenerRuntimeState::Listening
        );

        shutdown_tx.send(true).expect("listener shutdown");
        service_task.await.expect("listener service task");
        assert_eq!(
            runtime
                .internal_listener_snapshots()
                .expect("stopped snapshot")[0]
                .state,
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
            runtime
                .internal_listener_snapshots()
                .expect("failed snapshot")[0]
                .state,
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
            runtime
                .internal_listener_snapshots()
                .expect("failed snapshot")[0]
                .state,
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
