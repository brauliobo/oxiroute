use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use oxiroute_config::ConfigDraft;
use oxiroute_config_source::ConfigFormat;
use pingora::{
    apps::ServerApp,
    protocols::{
        ALPN, GetProxyDigest, GetSocketDigest, GetTimingDigest, Peek, Shutdown as PingoraShutdown,
        SocketDigest, Ssl, Stream, TimingDigest, UniqueID,
    },
    server::ShutdownWatch,
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

use super::*;
use crate::{
    GenerationManager, RuntimeMetrics,
    config::coordinator::{AuthoredRevision, EffectiveRevision, ResolvedConfigDocument},
};

struct CleanupApp {
    cleaned: Arc<AtomicBool>,
}

#[derive(Debug)]
struct H2TestStream {
    inner: DuplexStream,
    shutdown: Arc<AtomicBool>,
}

impl AsyncRead for H2TestStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl AsyncWrite for H2TestStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[async_trait]
impl PingoraShutdown for H2TestStream {
    async fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

impl UniqueID for H2TestStream {
    fn id(&self) -> pingora::protocols::UniqueIDType {
        0
    }
}

impl Ssl for H2TestStream {
    fn selected_alpn_proto(&self) -> Option<ALPN> {
        Some(ALPN::H2)
    }
}

impl GetTimingDigest for H2TestStream {
    fn get_timing_digest(&self) -> Vec<Option<TimingDigest>> {
        Vec::new()
    }
}

impl GetProxyDigest for H2TestStream {
    fn get_proxy_digest(&self) -> Option<Arc<pingora::protocols::raw_connect::ProxyDigest>> {
        None
    }
}

impl GetSocketDigest for H2TestStream {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        None
    }
}

impl Peek for H2TestStream {}

struct ProcessProbe {
    called: Arc<AtomicBool>,
}

struct RejectingAdmissionApp;

#[async_trait]
impl ServerApp for ProcessProbe {
    async fn process_new(
        self: &Arc<Self>,
        _session: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        self.called.store(true, Ordering::Release);
        None
    }
}

#[async_trait]
impl ServerApp for CleanupApp {
    async fn process_new(
        self: &Arc<Self>,
        _session: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        None
    }

    async fn cleanup(&self) {
        self.cleaned.store(true, Ordering::Relaxed);
    }
}

#[async_trait]
impl ServerApp for RejectingAdmissionApp {
    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        None
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        None
    }

    async fn process_new(
        self: &Arc<Self>,
        _session: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        None
    }
}

fn generation() -> Arc<RuntimeGeneration> {
    let config = ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        worker_scaling: None,
        min_active_workers: None,
        target_busy_percent: None,
        scale_down_delay_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
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
        audit: None,
        log_filter: None,
    }
    .validate()
    .expect("valid HTTP test config");
    let manager = GenerationManager::new();
    let candidate = manager
        .prepare(ResolvedConfigDocument {
            authored_revision: AuthoredRevision::from_bytes(b"http-test"),
            effective_revision: EffectiveRevision::from_bytes(b"http-test"),
            validated_config: config,
            format: ConfigFormat::Lua,
            compositional: false,
            dependencies: Vec::new(),
            config_preview: String::new(),
            diagnostics: Vec::new(),
        })
        .expect("prepared HTTP test generation");
    manager
        .activate(&candidate)
        .expect("active HTTP test generation")
}

#[tokio::test]
async fn monitored_http_app_delegates_cleanup() {
    let cleaned = Arc::new(AtomicBool::new(false));
    let runtime = RuntimeMetrics::new();
    let metrics = runtime
        .register_listener("http", "http", "127.0.0.1:8080", 100)
        .expect("listener metrics");
    let app = MonitoredHttpApp::new(
        CleanupApp {
            cleaned: Arc::clone(&cleaned),
        },
        metrics,
        generation(),
    );

    app.cleanup().await;

    assert!(cleaned.load(Ordering::Relaxed));
}

#[tokio::test]
async fn h2_process_new_closes_when_generation_reference_is_unavailable() {
    let generation = generation();
    generation.stop_accepting();

    let called = Arc::new(AtomicBool::new(false));
    let app = Arc::new(
        HttpListenerApp::new(
            ProcessProbe {
                called: Arc::clone(&called),
            },
            None,
        )
        .with_generation(generation),
    );
    let (shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
    drop(shutdown_tx);
    let (stream, _peer) = tokio::io::duplex(16);
    let closed = Arc::new(AtomicBool::new(false));
    let result = app
        .process_new(
            Box::new(H2TestStream {
                inner: stream,
                shutdown: Arc::clone(&closed),
            }),
            &shutdown,
        )
        .await;

    assert!(result.is_none());
    assert!(closed.load(Ordering::Acquire));
    assert!(!called.load(Ordering::Acquire));
}

#[tokio::test]
async fn http1_connections_share_one_signal_that_fires_when_the_generation_is_replaced() {
    let generation = generation();
    let called = Arc::new(AtomicBool::new(false));
    let app = HttpListenerApp::new(ProcessProbe { called }, None)
        .with_generation(Arc::clone(&generation));
    let (_process, process_shutdown) = tokio::sync::watch::channel(false);

    let mut first = app.http1_shutdown(&process_shutdown);
    let mut second = app.http1_shutdown(&process_shutdown);
    assert!(!*first.borrow() && !*second.borrow());

    generation.mark_retired();
    first.changed().await.expect("first connection is told");
    second.changed().await.expect("second connection is told");
    assert!(*first.borrow() && *second.borrow());

    let late = app.http1_shutdown(&process_shutdown);
    assert!(
        *late.borrow(),
        "a connection opened after the replacement is already told"
    );
}

#[test]
fn reverse_http_admission_rolls_back_when_inner_admission_rejects() {
    let generation = generation();
    let metrics = RuntimeMetrics::with_max_connections(Some(1));
    let listener = metrics
        .register_listener("http", "http", "127.0.0.1:8080", 1)
        .expect("listener metrics");
    let app = MonitoredHttpApp::new(RejectingAdmissionApp, listener, Arc::clone(&generation));

    assert!(app.admit_connection().is_none());
    assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 0);
    let snapshot = metrics.snapshot().expect("rollback snapshot");
    assert_eq!(snapshot.traffic.active_connections, 0);
    assert!(snapshot.access_records.is_empty());
}

#[test]
fn reverse_http_owned_admission_retains_generation_drain_lifetime() {
    let generation = generation();
    let metrics = RuntimeMetrics::with_max_connections(Some(1));
    let listener = metrics
        .register_listener("http", "http", "127.0.0.1:8080", 1)
        .expect("listener metrics");
    let app = MonitoredHttpApp::new(
        ProcessProbe {
            called: Arc::new(AtomicBool::new(false)),
        },
        listener,
        Arc::clone(&generation),
    );

    generation.stop_accepting();
    let lease = app
        .admit_owned_connection()
        .expect("accept-gate-owned reverse HTTP admission");
    assert!(!generation.drain(Duration::ZERO));

    drop(lease);
    assert!(generation.drain(Duration::from_millis(100)));
    assert_eq!(
        metrics
            .snapshot()
            .expect("released snapshot")
            .traffic
            .active_connections,
        0
    );
}
