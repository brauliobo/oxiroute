use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use async_trait::async_trait;
use log::warn;
use oxiroute_config::DownstreamTimeoutPolicy;
use pingora::{
    apps::{
        AcceptGate, ConnectionAdmission, Http1Front, HttpServerApp, HttpServerOptions,
        ReusedHttpStream, ServerApp,
    },
    protocols::{
        ALPN, Stream,
        http::{ServerSession, v2::server::H2Options},
    },
    server::ShutdownWatch,
};
use tokio::sync::watch;

use crate::HttpServicePlan;
use crate::http::proxy::lane::{LaneFront, LaneTimeouts};
use crate::listener::runtime::{AdmissionError, ListenerRuntime};
use crate::{ListenerMetrics, RuntimeGeneration, RuntimeReferenceKind, TlsProfilePlan};
use oxiroute_acme::ChallengeStore;

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

/// Merges generation retirement and process shutdown into one signal that every HTTP/1
/// connection of a service shares, instead of a task and a channel per connection.
fn retirement_signal(
    generation: &RuntimeGeneration,
    process_shutdown: &ShutdownWatch,
) -> ShutdownWatch {
    let mut retired = generation.retirement_signal();
    let mut process_shutdown = process_shutdown.clone();
    let (shutdown_tx, shutdown_rx) =
        watch::channel(*process_shutdown.borrow() || *retired.borrow());
    tokio::spawn(async move {
        tokio::select! {
            _ = retired.wait_for(|retired| *retired) => {}
            _ = process_shutdown.wait_for(|stopping| *stopping) => {}
        }
        let _ = shutdown_tx.send(true);
    });
    shutdown_rx
}

fn log_admission_error(protocol: &str, error: &AdmissionError) {
    warn!("rejected {protocol} connection: {error}");
}

/// Enforces listener protocol policy on a negotiated transport before HTTP parsing begins.
pub struct HttpListenerApp<A> {
    generation: Option<Arc<RuntimeGeneration>>,
    retirement: OnceLock<ShutdownWatch>,
    inner: Arc<A>,
    h2_only: bool,
}

impl<A> HttpListenerApp<A> {
    #[must_use]
    pub fn new(inner: A, tls_profile: Option<&TlsProfilePlan>) -> Self {
        Self {
            generation: None,
            retirement: OnceLock::new(),
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

impl<A> HttpListenerApp<A> {
    /// A replaced generation closes its idle HTTP/1 connections and lets a busy one finish its
    /// request with `Connection: close`.
    fn http1_shutdown(&self, process_shutdown: &ShutdownWatch) -> ShutdownWatch {
        let Some(generation) = self.generation.as_ref() else {
            return process_shutdown.clone();
        };
        let mut signal = self
            .retirement
            .get_or_init(|| retirement_signal(generation, process_shutdown))
            .clone();
        if *signal.borrow() {
            signal.mark_unchanged();
        }
        signal
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
        let result = if let Some(h2_shutdown) = h2_shutdown.as_ref() {
            self.inner.process_new(downstream, h2_shutdown).await
        } else {
            let shutdown = self.http1_shutdown(shutdown);
            self.inner.process_new(downstream, &shutdown).await
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
    front: Option<LaneFront>,
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
            front: None,
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

impl<A> HttpDownstreamPolicyApp<A> {
    /// Serves the requests the fast lane can on the connection before a session exists.
    #[must_use]
    pub fn with_lane(
        mut self,
        service: Arc<HttpServicePlan>,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
        challenge: Option<ChallengeStore>,
    ) -> Self {
        let timeouts = LaneTimeouts {
            request: self.request_timeout,
            write: self.write_timeout,
            keepalive: self.keepalive_timeout,
        };
        self.front = LaneFront::new(service, metrics, generation, challenge, timeouts);
        self
    }
}

#[async_trait]
impl<A> HttpServerApp for HttpDownstreamPolicyApp<A>
where
    A: HttpServerApp + Send + Sync + 'static,
{
    async fn process_new_http(
        self: &Arc<Self>,
        session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        self.process_new_http_inline(session, shutdown).await
    }

    // Written out so the trait's `Send` bound is stated, which `async fn` cannot do for a trait method.
    #[allow(clippy::manual_async_fn)]
    fn process_new_http_inline(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> impl std::future::Future<Output = Option<ReusedHttpStream>> + Send {
        async move {
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
            self.inner.process_new_http_inline(session, shutdown).await
        }
    }

    fn http1_front(&self) -> Option<&dyn Http1Front> {
        self.front.as_ref().map(|front| front as &dyn Http1Front)
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
mod tests;
