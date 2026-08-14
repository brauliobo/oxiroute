use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use log::warn;
use oxiroute_config::DownstreamTimeoutPolicy;
use pingora::{
    apps::{
        AcceptGate, ConnectionAdmission, HttpServerApp, HttpServerOptions, ReusedHttpStream,
        ServerApp,
    },
    protocols::{
        ALPN, Stream,
        http::{ServerSession, v2::server::H2Options},
    },
    server::ShutdownWatch,
};
use tokio::sync::watch;

use crate::listener_runtime::{AdmissionError, ListenerRuntime};
use crate::{ListenerMetrics, RuntimeGeneration, RuntimeReferenceKind, TlsProfilePlan};

pub struct MonitoredHttpApp<A> {
    generation: Arc<RuntimeGeneration>,
    inner: Arc<A>,
    listener: ListenerRuntime,
}

impl<A> MonitoredHttpApp<A> {
    #[must_use]
    pub fn new(inner: A, metrics: ListenerMetrics, generation: Arc<RuntimeGeneration>) -> Self {
        Self {
            generation,
            inner: Arc::new(inner),
            listener: ListenerRuntime::new(metrics),
        }
    }
}

#[async_trait]
impl<A> ServerApp for MonitoredHttpApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.listener.accepting() && self.generation.accepting() && self.inner.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .listener
            .admit(&self.generation, RuntimeReferenceKind::Http1)
        {
            Ok(lease) => lease,
            Err(error) => {
                log_admission_error("HTTP", &error);
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((lease, inner)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .listener
            .admit_owned(&self.generation, RuntimeReferenceKind::Http1)
        {
            Ok(lease) => lease,
            Err(error) => {
                log_admission_error("HTTP", &error);
                return None;
            }
        };
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((lease, inner)))
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

fn log_admission_error(protocol: &str, error: &AdmissionError) {
    warn!("rejected {protocol} connection: {error}");
}

/// Enforces listener protocol policy on a negotiated transport before HTTP parsing begins.
pub struct HttpListenerApp<A> {
    generation: Option<Arc<RuntimeGeneration>>,
    inner: Arc<A>,
    h2_only: bool,
}

impl<A> HttpListenerApp<A> {
    #[must_use]
    pub fn new(inner: A, tls_profile: Option<&TlsProfilePlan>) -> Self {
        Self {
            generation: None,
            inner: Arc::new(inner),
            h2_only: tls_profile.is_some_and(TlsProfilePlan::is_h2_only),
        }
    }

    #[must_use]
    pub fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }
}

#[async_trait]
impl<A> ServerApp for HttpListenerApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.inner.accept_gate()
    }

    fn accepting(&self) -> bool {
        self.inner.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_connection()
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_owned_connection()
    }

    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        if self.h2_only && !matches!(downstream.selected_alpn_proto(), Some(ALPN::H2)) {
            // A ClientHello without ALPN completes TLS, so close before Pingora's HTTP/1 fallback.
            downstream.shutdown().await;
            return None;
        }
        // H2 has its own connection lifetime after HTTP/1 accept admission and retains this
        // protocol reference to drive generation GOAWAY and drain independently.
        let h2_reference = if matches!(downstream.selected_alpn_proto(), Some(ALPN::H2)) {
            if let Some(generation) = self.generation.as_ref() {
                let Some(reference) = generation.begin_reference(RuntimeReferenceKind::Http2)
                else {
                    downstream.shutdown().await;
                    return None;
                };
                Some(reference)
            } else {
                None
            }
        } else {
            None
        };
        let (h2_shutdown, h2_monitor) = if h2_reference.is_some() {
            let generation = self
                .generation
                .as_ref()
                .expect("H2 reference has a generation");
            let mut gate = generation.accept_gate().register();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let mut process_shutdown = shutdown.clone();
            let gate_state = gate.state();
            if *process_shutdown.borrow() || !gate_state.accepting {
                let _ = shutdown_tx.send(true);
                if !gate_state.accepting {
                    gate.acknowledge(gate_state.epoch);
                }
            }
            let monitor = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        changed = gate.changed() => {
                            let Ok(state) = changed else { break };
                            if !state.accepting {
                                let _ = shutdown_tx.send(true);
                                gate.acknowledge(state.epoch);
                                break;
                            }
                        }
                        changed = process_shutdown.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            if *process_shutdown.borrow() {
                                let _ = shutdown_tx.send(true);
                                break;
                            }
                        }
                    }
                }
            });
            (Some(shutdown_rx), Some(monitor))
        } else {
            (None, None)
        };
        let result = match h2_shutdown.as_ref() {
            Some(h2_shutdown) => self.inner.process_new(downstream, h2_shutdown).await,
            None => self.inner.process_new(downstream, shutdown).await,
        };
        if let Some(monitor) = h2_monitor {
            monitor.abort();
            let _ = monitor.await;
        }
        drop(h2_reference);
        result
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

pub struct HttpDownstreamPolicyApp<A> {
    inner: Arc<A>,
    request_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    keepalive_timeout: Option<Duration>,
}

impl<A> HttpDownstreamPolicyApp<A> {
    #[must_use]
    pub fn new(inner: A, policy: DownstreamTimeoutPolicy) -> Self {
        let client_timeout = policy.client_timeout_ms.map(Duration::from_millis);
        Self {
            inner: Arc::new(inner),
            request_timeout: policy
                .request_timeout_ms
                .map(Duration::from_millis)
                .or(client_timeout),
            write_timeout: client_timeout,
            keepalive_timeout: policy.keepalive_timeout_ms.map(Duration::from_millis),
        }
    }
}

#[async_trait]
impl<A> HttpServerApp for HttpDownstreamPolicyApp<A>
where
    A: HttpServerApp + Send + Sync + 'static,
{
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        let write_timeout = match (self.request_timeout, self.write_timeout) {
            (Some(request), Some(write)) => Some(request.min(write)),
            (request, None) => request,
            (None, write) => write,
        };
        session.set_read_timeout(self.request_timeout);
        session.set_write_timeout(write_timeout);
        session.set_request_header_timeout(self.request_timeout);
        session.set_idle_keepalive_timeout(self.keepalive_timeout);
        if self.keepalive_timeout.is_some() {
            session.set_keepalive(Some(0));
        }
        self.inner.process_new_http(session, shutdown).await
    }

    fn h2_options(&self) -> Option<H2Options> {
        self.inner.h2_options()
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        self.inner.server_options()
    }

    async fn http_cleanup(&self) {
        self.inner.http_cleanup().await;
    }
}

#[cfg(test)]
mod tests {
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
            ALPN, GetProxyDigest, GetSocketDigest, GetTimingDigest, Peek,
            Shutdown as PingoraShutdown, SocketDigest, Ssl, Stream, TimingDigest, UniqueID,
        },
        server::ShutdownWatch,
    };
    use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

    use super::*;
    use crate::{
        GenerationManager, RuntimeMetrics,
        config_coordinator::{AuthoredRevision, EffectiveRevision, ResolvedConfigDocument},
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
}
