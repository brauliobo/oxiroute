use std::{
    collections::HashMap,
    error::Error,
    io,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use futures_util::FutureExt as _;
use log::{error, info, warn};
use oxiroute_config::ListenerBind;
use oxiroute_rtmp::{MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, RtmpRegistry, RtmpServiceRuntime};
use oxiroute_server::{
    CertbotWatcherConfig, CertbotWatcherSupervisor, HttpListenerApp, HttpReverseProxy,
    ListenerMetrics, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RtmpManagementApi, RuntimeMetrics,
    RuntimePlan, ServiceKind, TcpRelayCore, TlsProfilePlan, TopologySnapshot,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigRevision},
    runtime_plan,
};
use pingora::{
    apps::http_app::HttpServer,
    apps::{ConnectionAdmission, ServerApp},
    protocols::Stream,
    proxy::http_proxy,
    server::{RunArgs, Server, ShutdownWatch, configuration::ServerConf},
    services::{
        Service as PingoraService, ServiceReadyNotifier, ServiceWithDependents,
        background::background_service, listening::Service,
    },
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
        ListenerBind::Unix { path } => {
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

#[cfg(unix)]
struct ListenerReservation {
    bind: String,
    socket: Option<ReservedSocket>,
    unix_socket: Option<UnixSocketIdentity>,
}

#[cfg(unix)]
enum ReservedSocket {
    Tcp(std::net::TcpListener),
    Unix(std::os::unix::net::UnixListener),
}

#[cfg(unix)]
struct UnixSocketIdentity {
    device: u64,
    inode: u64,
    path: PathBuf,
}

#[cfg(unix)]
impl UnixSocketIdentity {
    fn remove_if_unchanged(&self) {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            if let Err(source) = std::fs::remove_file(&self.path) {
                warn!(
                    "could not remove stopped Unix listener socket `{}`: {source}",
                    self.path.display()
                );
            }
        }
    }
}

#[cfg(unix)]
impl ListenerReservation {
    fn into_fds(mut self) -> (pingora::server::Fds, Option<UnixSocketIdentity>) {
        use std::os::fd::IntoRawFd as _;

        let mut fds = pingora::server::Fds::new();
        let fd = match self
            .socket
            .take()
            .expect("reserved listener socket is transferred exactly once")
        {
            ReservedSocket::Tcp(listener) => listener.into_raw_fd(),
            ReservedSocket::Unix(listener) => listener.into_raw_fd(),
        };
        fds.add(std::mem::take(&mut self.bind), fd);
        (fds, self.unix_socket.take())
    }
}

#[cfg(unix)]
impl Drop for ListenerReservation {
    fn drop(&mut self) {
        if let Some(unix_socket) = &self.unix_socket {
            unix_socket.remove_if_unchanged();
        }
    }
}

#[cfg(not(unix))]
struct ListenerReservation;

fn reserve_listener(listener_name: &str, bind: &ListenerBind) -> io::Result<ListenerReservation> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        match bind {
            ListenerBind::Socket { address } => {
                let listener = std::net::TcpListener::bind(address).map_err(|source| {
                    io::Error::new(
                        source.kind(),
                        format!(
                            "listener `{listener_name}` could not bind socket `{address}`: {source}"
                        ),
                    )
                })?;
                listener.set_nonblocking(true)?;
                Ok(ListenerReservation {
                    bind: address.to_string(),
                    socket: Some(ReservedSocket::Tcp(listener)),
                    unix_socket: None,
                })
            }
            ListenerBind::Udp { address } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("listener `{listener_name}` cannot reserve UDP socket `{address}` yet"),
            )),
            ListenerBind::Unix { path } => {
                let path_text = path.to_str().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "listener `{listener_name}` Unix socket path is not valid UTF-8 and cannot be bound"
                        ),
                    )
                })?;
                let listener = std::os::unix::net::UnixListener::bind(path).map_err(|source| {
                    io::Error::new(
                        source.kind(),
                        format!(
                            "listener `{listener_name}` could not bind Unix socket `{path_text}`: {source}"
                        ),
                    )
                })?;
                listener.set_nonblocking(true)?;
                let metadata = std::fs::symlink_metadata(path)?;
                Ok(ListenerReservation {
                    bind: path_text.to_owned(),
                    socket: Some(ReservedSocket::Unix(listener)),
                    unix_socket: Some(UnixSocketIdentity {
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        path: path.clone(),
                    }),
                })
            }
        }
    }
    #[cfg(not(unix))]
    {
        match bind {
            ListenerBind::Socket { .. } => Ok(ListenerReservation),
            ListenerBind::Udp { .. } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("listener `{listener_name}` uses UDP on an unsupported runtime"),
            )),
            ListenerBind::Unix { .. } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("listener `{listener_name}` uses a Unix socket on an unsupported platform"),
            )),
        }
    }
}

struct RuntimeListenerService<S> {
    inner: S,
    metrics: Option<ListenerMetrics>,
    reservation: Option<ListenerReservation>,
}

impl<S> RuntimeListenerService<S> {
    fn new(inner: S, reservation: ListenerReservation, metrics: Option<ListenerMetrics>) -> Self {
        Self {
            inner,
            metrics,
            reservation: Some(reservation),
        }
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
        #[cfg(unix)]
        let (fds, unix_socket) = self
            .reservation
            .take()
            .expect("listener service starts exactly once")
            .into_fds();
        #[cfg(unix)]
        let fds = Some(Arc::new(tokio::sync::Mutex::new(fds)));
        #[cfg(not(unix))]
        let _reservation = self
            .reservation
            .take()
            .expect("listener service starts exactly once");

        if let Some(metrics) = &self.metrics {
            metrics.mark_listening();
        }
        ready_notifier.notify_ready();
        let result = AssertUnwindSafe(PingoraService::start_service(
            &mut self.inner,
            #[cfg(unix)]
            fds,
            shutdown,
            listeners_per_fd,
        ))
        .catch_unwind()
        .await;
        if let Some(metrics) = &self.metrics {
            if result.is_ok() {
                metrics.mark_stopped();
            } else {
                metrics.mark_failed();
            }
        }
        #[cfg(unix)]
        if let Some(unix_socket) = unix_socket {
            unix_socket.remove_if_unchanged();
        }
        if result.is_err() {
            error!("listener service `{}` terminated unexpectedly", self.name());
        }
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn threads(&self) -> Option<usize> {
        self.inner.threads()
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

struct TcpRelay {
    service: Arc<oxiroute_server::L4ServicePlan>,
    metrics: ListenerMetrics,
}

impl TcpRelay {
    fn new(service: Arc<oxiroute_server::L4ServicePlan>, metrics: ListenerMetrics) -> Self {
        Self { service, metrics }
    }
}

#[async_trait]
impl ServerApp for TcpRelay {
    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        admit_connection(&self.metrics)
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let connection = self.metrics.traffic_accounting();
        let Some(upstream) = self.service.select() else {
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
    metrics: ListenerMetrics,
    runtime: RtmpServiceRuntime,
}

impl RtmpIngest {
    fn new(runtime: RtmpServiceRuntime, metrics: ListenerMetrics) -> Self {
        Self { metrics, runtime }
    }
}

#[async_trait]
impl ServerApp for RtmpIngest {
    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        admit_connection(&self.metrics)
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

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OxiRoute startup failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run() -> Result<(), Box<dyn Error>> {
    let config_path = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "oxiroute.lua".into());
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
    let active_revision = config_document.disk_revision.clone();
    let config = config_document.normalized_config;
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
        ..ServerConf::default()
    };
    let mut server = Server::new_with_opt_and_conf(None, server_config);
    server.bootstrap();

    let RuntimePlan {
        services,
        health_supervisor,
        pools,
        rtmp_capabilities,
        rtmp_recording_supported,
        tls,
        topology,
    } = runtime_plan(&config)?;
    let certbot_reconcilers = tls.certbot_reconcilers().to_vec();
    if let Some(supervisor) = health_supervisor {
        server.add_service(background_service("upstream health", supervisor));
    }
    let runtime_metrics = RuntimeMetrics::new();
    runtime_metrics.register_upstream_pools(pools)?;
    let mut monitored_services = Vec::with_capacity(services.len());
    for spec in services {
        let metrics = runtime_metrics.register_configured_listener(
            &spec.name,
            spec.kind.protocol(),
            &spec.bind,
            spec.max_connections,
        )?;
        let reservation = reserve_listener(&spec.name, &spec.bind)?;
        monitored_services.push((spec, metrics, reservation));
    }
    let services = monitored_services;
    let management_reservation = config
        .management
        .as_ref()
        .map(|management| {
            reserve_listener(
                "management",
                &ListenerBind::Socket {
                    address: management.bind,
                },
            )
        })
        .transpose()?;
    let rtmp_registry = Arc::new(RtmpRegistry::new(rtmp_capabilities));
    let mut rtmp_runtimes = HashMap::new();
    for (spec, _, _) in &services {
        let ServiceKind::Rtmp(plan) = &spec.kind else {
            continue;
        };
        if !rtmp_runtimes.contains_key(plan.service_id()) {
            let runtime = plan.runtime(Arc::clone(&rtmp_registry))?;
            rtmp_runtimes.insert(plan.service_id().to_owned(), runtime);
        }
    }
    runtime_metrics.set_rtmp_recording_supported(rtmp_recording_supported);
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
        )?;
        let app = HttpListenerApp::new(HttpServer::new_app(management_api), None);
        let mut service = Service::new("OxiRoute management".into(), app);
        service.add_tcp(&management.bind.to_string());
        server.add_service(RuntimeListenerService::new(
            service,
            management_reservation.expect("management listener reservation was prepared above"),
            None,
        ));
        info!("configured management API on {}", management.bind);
    }

    for (spec, metrics, reservation) in services {
        let listener_name = spec.name;
        let listener_bind = spec.bind;
        let service_name = format!("OxiRoute {listener_name}");
        let listener_tls = spec.tls;

        match spec.kind {
            ServiceKind::Http(http_service) => {
                let proxy = http_proxy(
                    &server.configuration,
                    HttpReverseProxy::new(http_service, metrics.clone()),
                );
                let app = MonitoredHttpApp::new(
                    HttpListenerApp::new(proxy, listener_tls.as_deref()),
                    metrics.clone(),
                );
                let mut service = Service::new(service_name, app);
                add_http_listener(
                    &mut service,
                    &listener_name,
                    &listener_bind,
                    listener_tls.as_deref(),
                )?;
                server.add_service(RuntimeListenerService::new(
                    service,
                    reservation,
                    Some(metrics),
                ));
            }
            ServiceKind::Rtmp(rtmp_service) => {
                let runtime = rtmp_runtimes
                    .get(rtmp_service.service_id())
                    .expect("RTMP runtimes were prepared before listener registration")
                    .clone();
                let mut service =
                    Service::new(service_name, RtmpIngest::new(runtime, metrics.clone()));
                add_plain_listener(&mut service, &listener_name, &listener_bind)?;
                server.add_service(RuntimeListenerService::new(
                    service,
                    reservation,
                    Some(metrics),
                ));
            }
            ServiceKind::Tcp(l4_service) => {
                let mut service =
                    Service::new(service_name, TcpRelay::new(l4_service, metrics.clone()));
                add_plain_listener(&mut service, &listener_name, &listener_bind)?;
                server.add_service(RuntimeListenerService::new(
                    service,
                    reservation,
                    Some(metrics),
                ));
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
    server.run(RunArgs::default());
    if let Some(watcher) = &mut certbot_watcher {
        watcher.shutdown();
    }
    drop(tls);
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
    use oxiroute_config::{
        Config, HealthCheck, HealthCheckType, HttpVersionPolicy, L4Service, Listener, ListenerBind,
        Protocol, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool,
    };
    use oxiroute_server::runtime_plan;
    use tokio::{net::TcpListener, sync::watch};

    use super::*;

    #[tokio::test]
    async fn tcp_handler_closes_connections_when_its_pool_is_unavailable() {
        let ingress = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ingress bind");
        let ingress_address = ingress.local_addr().expect("ingress address");
        let config = Config {
            version: 1,
            management: None,
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
            }],
            upstream_pools: vec![UpstreamPool {
                name: "database".into(),
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
                    host: None,
                    path: None,
                }),
                tls: None,
                http_versions: HttpVersionPolicy::default(),
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
        use tokio::net::UnixStream;

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("listener.sock");
        let bind = ListenerBind::Unix { path: path.clone() };
        let reservation = reserve_listener("unix", &bind).expect("Unix listener reservation");
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
        let bind = ListenerBind::Unix { path: path.clone() };
        let error = reserve_listener("unix", &bind)
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
        };

        let error = reserve_listener("invalid", &bind)
            .err()
            .expect("non-UTF-8 Unix listener path must fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("not valid UTF-8"));
    }
}
