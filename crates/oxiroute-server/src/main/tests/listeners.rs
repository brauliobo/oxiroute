#[allow(clippy::wildcard_imports)]
use super::*;
use std::path::PathBuf;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn tcp_handler_closes_connections_when_its_pool_is_unavailable() {
    let ingress = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("ingress bind");
    let ingress_address = ingress.local_addr().expect("ingress address");
    let config = ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
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
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
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
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: Vec::new(),
        cache_stores: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: vec![L4Service {
            drain_deadline_ms: None,
            name: "database".into(),
            upstream_pool: "database".into(),
            connect_timeout_ms: 100,
            idle_timeout_ms: 1_000,
            lifetime_timeout_ms: None,
            proxy_protocol: None,
            udp: None,
        }],
        audit: None,
        log_filter: None,
    };
    let directory = tempfile::tempdir().expect("TCP generation directory");
    let mut generation_config = config.clone();
    generation_config.listeners.clear();
    let (_, _, generation) =
        activate_test_generation(&directory.path().join("oxiroute.kdl"), &generation_config);
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
    let app = Arc::new(TcpRelay::new(
        service,
        listener_metrics,
        None,
        Arc::clone(&generation),
    ));
    let mut client = tokio::net::TcpStream::connect(ingress_address)
        .await
        .expect("client connect");
    let (downstream, _) = ingress.accept().await.expect("ingress accept");
    let downstream: Stream = Box::new(pingora::protocols::l4::stream::Stream::from(downstream));
    let (_shutdown_tx, shutdown) = watch::channel(false);
    assert!(app.accept_gate().is_some());
    let admission = app.admit_connection().expect("connection admission");
    assert_eq!(generation.active_references(RuntimeReferenceKind::Tcp), 1);
    drop(admission);
    let admission = app
        .admit_owned_connection()
        .expect("owned connection admission");
    generation.stop_accepting();
    assert!(!generation.drain(Duration::ZERO));

    assert!(app.process_new(downstream, &shutdown).await.is_none());
    drop(admission);
    assert!(generation.drain(Duration::from_millis(100)));
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.expect("client EOF");
    assert!(response.is_empty());
    let snapshot = metrics.snapshot().expect("runtime snapshot");
    assert_eq!(snapshot.traffic.accepted_connections, 2);
    assert_eq!(snapshot.traffic.active_connections, 0);
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
    let reservation = ListenerReservation::bind("unix", &bind).expect("Unix listener reservation");
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
