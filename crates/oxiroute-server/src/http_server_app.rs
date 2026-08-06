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
        http::{v2::server::H2Options, ServerSession},
        Stream, ALPN,
    },
    server::ShutdownWatch,
};
use tokio::sync::watch;

use crate::{ListenerMetrics, RuntimeGeneration, RuntimeReferenceKind, TlsProfilePlan};

pub struct MonitoredHttpApp<A> {
    generation: Option<Arc<RuntimeGeneration>>,
    inner: Arc<A>,
    metrics: ListenerMetrics,
}

impl<A> MonitoredHttpApp<A> {
    #[must_use]
    pub fn new(inner: A, metrics: ListenerMetrics) -> Self {
        Self {
            generation: None,
            inner: Arc::new(inner),
            metrics,
        }
    }

    #[must_use]
    pub fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }
}

#[async_trait]
impl<A> ServerApp for MonitoredHttpApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.generation.as_ref().map_or_else(
            || self.inner.accept_gate(),
            |generation| Some(generation.accept_gate()),
        )
    }

    fn accepting(&self) -> bool {
        self.metrics.accepting()
            && self.generation.as_ref().map_or_else(
                || self.inner.accepting(),
                |generation| generation.accepting(),
            )
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let generation = if let Some(generation) = &self.generation {
            let admission = generation.begin_admission()?;
            let reference = generation.begin_reference(RuntimeReferenceKind::Http1)?;
            Some((admission, reference))
        } else {
            None
        };
        let connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected HTTP connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((generation, connection, inner)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let generation = self
            .generation
            .as_ref()
            .map(|generation| generation.begin_owned_reference(RuntimeReferenceKind::Http1));
        let connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected HTTP connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((generation, connection, inner)))
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
        let h2_reference = if matches!(downstream.selected_alpn_proto(), Some(ALPN::H2)) {
            self.generation
                .as_ref()
                .and_then(|generation| generation.begin_reference(RuntimeReferenceKind::Http2))
        } else {
            None
        };
        if self.h2_only && !matches!(downstream.selected_alpn_proto(), Some(ALPN::H2)) {
            // A ClientHello without ALPN completes TLS, so close before Pingora's HTTP/1 fallback.
            downstream.shutdown().await;
            return None;
        }
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
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use pingora::{apps::ServerApp, protocols::Stream, server::ShutdownWatch};

    use super::*;
    use crate::RuntimeMetrics;

    struct CleanupApp {
        cleaned: Arc<AtomicBool>,
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
        );

        app.cleanup().await;

        assert!(cleaned.load(Ordering::Relaxed));
    }
}
