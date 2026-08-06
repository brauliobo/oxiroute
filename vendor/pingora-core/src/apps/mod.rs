// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The abstraction and implementation interface for service application logic

pub mod http_app;
pub mod prometheus_http_app;

use crate::server::ShutdownWatch;
use async_trait::async_trait;
use log::{debug, error};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use std::{any::Any, future::poll_fn};

use crate::protocols::http::v2::server;
use crate::protocols::http::ServerSession;
use crate::protocols::Digest;
use crate::protocols::Stream;
use crate::protocols::ALPN;

// https://datatracker.ietf.org/doc/html/rfc9113#section-3.4
const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// An application-owned guard retained for the complete transport connection lifetime.
pub type ConnectionAdmission = Box<dyn Any + Send + Sync>;

#[derive(Clone)]
/// Coordinates exclusive accept ownership across listener tasks during runtime handoff.
pub struct AcceptGate {
    inner: Arc<AcceptGateInner>,
}

struct AcceptGateInner {
    changed: tokio::sync::watch::Sender<AcceptGateState>,
    quiesced: Condvar,
    state: Mutex<AcceptGateInnerState>,
    #[cfg(test)]
    publish_hook: Mutex<Option<AcceptGatePublishHook>>,
}

#[cfg(test)]
type AcceptGatePublishHook = Arc<dyn Fn(AcceptGateState) + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptGateState {
    pub accepting: bool,
    pub epoch: u64,
}

struct AcceptGateInnerState {
    claims: usize,
    next_participant_id: u64,
    participants: HashMap<u64, u64>,
    published: AcceptGateState,
}

/// Identifies one gate close so only that close can restore acceptance.
pub struct AcceptGateClose {
    gate: AcceptGate,
    epoch: u64,
    reopen_allowed: bool,
}

impl AcceptGate {
    #[must_use]
    pub fn closed() -> Self {
        let published = AcceptGateState {
            accepting: false,
            epoch: 0,
        };
        let (changed, _) = tokio::sync::watch::channel(published);
        Self {
            inner: Arc::new(AcceptGateInner {
                changed,
                quiesced: Condvar::new(),
                state: Mutex::new(AcceptGateInnerState {
                    claims: 0,
                    next_participant_id: 0,
                    participants: HashMap::new(),
                    published,
                }),
                #[cfg(test)]
                publish_hook: Mutex::new(None),
            }),
        }
    }

    #[must_use]
    pub fn accepting(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .published
            .accepting
    }

    #[must_use]
    pub fn register(&self) -> AcceptGateParticipant {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.next_participant_id = state
            .next_participant_id
            .checked_add(1)
            .expect("accept gate participant ID overflow");
        let id = state.next_participant_id;
        let acknowledged_epoch = if state.published.accepting {
            state.published.epoch.saturating_sub(1)
        } else {
            state.published.epoch
        };
        state.participants.insert(id, acknowledged_epoch);
        drop(state);
        AcceptGateParticipant {
            gate: self.clone(),
            id,
            watch: self.inner.changed.subscribe(),
        }
    }

    #[must_use]
    pub fn claim(&self) -> Option<AcceptOwnership> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.published.accepting {
            return None;
        }
        state.claims = state
            .claims
            .checked_add(1)
            .expect("accept ownership claim overflow");
        Some(AcceptOwnership { gate: self.clone() })
    }

    pub fn enable(&self) {
        let published = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            debug_assert_eq!(state.claims, 0, "accept gate enabled with live claims");
            debug_assert!(
                state
                    .participants
                    .values()
                    .all(|epoch| *epoch >= state.published.epoch),
                "accept gate enabled before participants quiesced"
            );
            state.published = AcceptGateState {
                accepting: true,
                epoch: state
                    .published
                    .epoch
                    .checked_add(1)
                    .expect("accept gate epoch overflow"),
            };
            state.published
        };
        self.publish(published);
    }

    #[must_use]
    pub fn close(&self) -> AcceptGateClose {
        let (published, reopen_allowed) = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let reopen_allowed = state.published.accepting;
            state.published = AcceptGateState {
                accepting: false,
                epoch: state
                    .published
                    .epoch
                    .checked_add(1)
                    .expect("accept gate epoch overflow"),
            };
            (state.published, reopen_allowed)
        };
        self.publish(published);
        AcceptGateClose {
            gate: self.clone(),
            epoch: published.epoch,
            reopen_allowed,
        }
    }

    #[must_use]
    pub fn close_and_wait(&self, timeout: Duration) -> bool {
        self.close().wait(timeout)
    }

    fn publish(&self, published: AcceptGateState) {
        #[cfg(test)]
        let hook = self
            .inner
            .publish_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        #[cfg(test)]
        if let Some(hook) = hook {
            hook(published);
        }
        // Publishing occurs outside the gate mutex. Epoch comparison prevents a delayed sender
        // from replacing a newer transition and avoids invoking wakeups under the gate lock.
        self.inner.changed.send_if_modified(|current| {
            if published.epoch <= current.epoch {
                return false;
            }
            *current = published;
            true
        });
    }

    #[cfg(test)]
    fn set_publish_hook(&self, hook: AcceptGatePublishHook) {
        *self
            .inner
            .publish_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }
}

impl AcceptGateClose {
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .gate
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if state.claims == 0
                && state
                    .participants
                    .values()
                    .all(|acknowledged| *acknowledged >= self.epoch)
            {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, timeout) = self
                .gate
                .inner
                .quiesced
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() {
                return state.claims == 0
                    && state
                        .participants
                        .values()
                        .all(|acknowledged| *acknowledged >= self.epoch);
            }
        }
    }

    #[must_use]
    pub fn is_current(&self) -> bool {
        let state = self
            .gate
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.published.accepting && state.published.epoch == self.epoch
    }

    /// Restores acceptance only when no later gate transition superseded this close.
    #[must_use]
    pub fn reopen(self) -> bool {
        if !self.reopen_allowed {
            return false;
        }
        let published = {
            let mut state = self
                .gate
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.published.accepting || state.published.epoch != self.epoch {
                return false;
            }
            state.published = AcceptGateState {
                accepting: true,
                epoch: state
                    .published
                    .epoch
                    .checked_add(1)
                    .expect("accept gate epoch overflow"),
            };
            state.published
        };
        self.gate.publish(published);
        true
    }
}

pub struct AcceptGateParticipant {
    gate: AcceptGate,
    id: u64,
    watch: tokio::sync::watch::Receiver<AcceptGateState>,
}

impl AcceptGateParticipant {
    #[must_use]
    pub fn state(&self) -> AcceptGateState {
        *self.watch.borrow()
    }

    pub async fn changed(
        &mut self,
    ) -> Result<AcceptGateState, tokio::sync::watch::error::RecvError> {
        self.watch.changed().await?;
        Ok(*self.watch.borrow_and_update())
    }

    #[must_use]
    pub fn claim(&self) -> Option<AcceptOwnership> {
        self.gate.claim()
    }

    pub fn acknowledge(&self, epoch: u64) {
        let mut state = self
            .gate
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(acknowledged) = state.participants.get_mut(&self.id) else {
            return;
        };
        *acknowledged = (*acknowledged).max(epoch);
        self.gate.inner.quiesced.notify_all();
    }
}

impl Drop for AcceptGateParticipant {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.participants.remove(&self.id);
        self.gate.inner.quiesced.notify_all();
    }
}

pub struct AcceptOwnership {
    gate: AcceptGate,
}

impl Drop for AcceptOwnership {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.claims = state
            .claims
            .checked_sub(1)
            .expect("accept ownership claim underflow");
        self.gate.inner.quiesced.notify_all();
    }
}

#[cfg(test)]
mod accept_gate_tests {
    use std::{
        sync::{mpsc, Arc, Barrier},
        thread,
        time::Duration,
    };

    use super::AcceptGate;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_waits_for_every_participant_and_ownership_claim() {
        let gate = AcceptGate::closed();
        gate.enable();
        let mut first = gate.register();
        let mut second = gate.register();
        let first_claim = first.claim().expect("first ownership claim");
        let second_claim = second.claim().expect("second ownership claim");
        let closing_gate = gate.clone();
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            closed_tx
                .send(closing_gate.close_and_wait(Duration::from_secs(1)))
                .expect("close result receiver");
        });

        let first_state = first.changed().await.expect("first close notification");
        let second_state = second.changed().await.expect("second close notification");
        assert!(!first_state.accepting);
        assert_eq!(first_state, second_state);
        assert!(closed_rx.try_recv().is_err());

        first.acknowledge(first_state.epoch);
        drop(first_claim);
        assert!(closed_rx.try_recv().is_err());

        second.acknowledge(second_state.epoch);
        assert!(closed_rx.try_recv().is_err());
        drop(second_claim);

        assert!(closed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("gate close result"));
        closer.join().expect("gate closer");
    }

    #[test]
    fn timed_out_close_can_be_reopened_without_waiting_for_a_live_claim() {
        let gate = AcceptGate::closed();
        gate.enable();
        let ownership = gate.claim().expect("ownership claim");

        let close = gate.close();
        assert!(!close.wait(Duration::ZERO));
        assert!(close.reopen());
        assert!(gate.accepting());
        assert!(gate.claim().is_some());

        drop(ownership);
    }

    #[test]
    fn close_cannot_reopen_after_a_later_close() {
        let gate = AcceptGate::closed();
        gate.enable();

        let activation_close = gate.close();
        let shutdown_close = gate.close();

        assert!(!activation_close.reopen());
        assert!(!shutdown_close.reopen());
        assert!(!gate.accepting());
    }

    #[test]
    fn delayed_close_publication_cannot_regress_the_current_epoch() {
        let gate = AcceptGate::closed();
        gate.enable();
        let participant = gate.register();
        let delayed_epoch = participant.state().epoch + 1;
        let release = Arc::new(Barrier::new(2));
        let hook_release = Arc::clone(&release);
        let (delayed, delayed_rx) = mpsc::sync_channel(1);
        gate.set_publish_hook(Arc::new(move |state| {
            if state.epoch == delayed_epoch {
                delayed.send(()).expect("delayed publication receiver");
                hook_release.wait();
            }
        }));
        let delayed_gate = gate.clone();
        let (close_tx, close_rx) = mpsc::sync_channel(1);
        let older = thread::spawn(move || {
            close_tx
                .send(delayed_gate.close())
                .expect("older close receiver");
        });
        delayed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("older publication did not pause");

        let current = gate.close();
        let current_state = participant.state();
        assert_eq!(current_state.epoch, current.epoch);
        assert!(!current_state.accepting);
        participant.acknowledge(current_state.epoch);

        release.wait();
        let older_close = close_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("older close");
        older.join().expect("older close thread");

        assert_eq!(participant.state(), current_state);
        assert!(current.is_current());
        assert!(current.wait(Duration::ZERO));
        assert!(!older_close.is_current());
    }
}

#[async_trait]
/// This trait defines the interface of a transport layer (TCP or TLS) application.
pub trait ServerApp {
    /// Returns the shared gate used to cancel and acknowledge this application's accept loops.
    fn accept_gate(&self) -> Option<AcceptGate> {
        None
    }

    /// Reports whether this application currently permits its listener to accept a transport.
    fn accepting(&self) -> bool {
        true
    }

    /// Returns the maximum time allowed for a transport handshake.
    fn handshake_timeout(&self) -> Duration {
        Duration::from_secs(60)
    }

    /// Acquires capacity before a transport handshake. Returning `None` rejects the connection.
    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        Some(Box::new(()))
    }

    /// Admits a connection while the listener still holds an [`AcceptOwnership`] claim.
    ///
    /// Implementations must not recheck the gate: publication waits for the claim to be converted
    /// into the returned connection-lifetime guard.
    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        self.admit_connection()
    }

    /// Whenever a new connection is established, this function will be called with the established
    /// [`Stream`] object provided.
    ///
    /// The application can do whatever it wants with the `session`.
    ///
    /// After processing the `session`, if the `session`'s connection is reusable, This function
    /// can return it to the service by returning `Some(session)`. The returned `session` will be
    /// fed to another [`Self::process_new()`] for another round of processing.
    /// If not reusable, `None` should be returned.
    ///
    /// The `shutdown` argument will change from `false` to `true` when the server receives a
    /// signal to shutdown. This argument allows the application to react accordingly.
    async fn process_new(
        self: &Arc<Self>,
        mut session: Stream,
        // TODO: make this ShutdownWatch so that all task can await on this event
        shutdown: &ShutdownWatch,
    ) -> Option<Stream>;

    /// This callback will be called once after the service stops listening to its endpoints.
    async fn cleanup(&self) {}
}
#[non_exhaustive]
#[derive(Default)]
/// HTTP Server options that control how the server handles some transport types.
pub struct HttpServerOptions {
    /// Allow HTTP/2 for plaintext.
    pub h2c: bool,

    /// Allow proxying CONNECT requests when handling HTTP traffic.
    ///
    /// When disabled, CONNECT requests are rejected with 405 by proxy services.
    pub allow_connect_method_proxying: bool,

    #[doc(hidden)]
    pub force_custom: bool,

    /// Maximum number of requests that this connection will handle. This is
    /// equivalent to [Nginx's keepalive requests](https://nginx.org/en/docs/http/ngx_http_upstream_module.html#keepalive_requests)
    /// which says:
    ///
    /// > Closing connections periodically is necessary to free per-connection
    /// > memory allocations. Therefore, using too high maximum number of
    /// > requests could result in excessive memory usage and not recommended.
    ///
    /// Unlike nginx, the default behavior here is _no limit_.
    pub keepalive_request_limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct HttpPersistentSettings {
    keepalive_timeout: Option<u64>,
    keepalive_reuses_remaining: Option<u32>,
}

impl HttpPersistentSettings {
    pub fn for_session(session: &ServerSession) -> Self {
        HttpPersistentSettings {
            keepalive_timeout: session.get_keepalive(),
            keepalive_reuses_remaining: session.get_keepalive_reuses_remaining(),
        }
    }

    pub fn apply_to_session(self, session: &mut ServerSession) {
        let Self {
            keepalive_timeout,
            mut keepalive_reuses_remaining,
        } = self;

        // Reduce the number of times the connection for this session can be
        // reused by one. A session with reuse count of zero won't be reused
        if let Some(reuses) = keepalive_reuses_remaining.as_mut() {
            *reuses = reuses.saturating_sub(1);
        }

        session.set_keepalive(keepalive_timeout);
        session.set_keepalive_reuses_remaining(keepalive_reuses_remaining);
        session.mark_reused_connection();
    }
}

#[derive(Debug)]
pub struct ReusedHttpStream {
    stream: Stream,
    persistent_settings: Option<HttpPersistentSettings>,
}

impl ReusedHttpStream {
    pub fn new(stream: Stream, persistent_settings: Option<HttpPersistentSettings>) -> Self {
        ReusedHttpStream {
            stream,
            persistent_settings,
        }
    }

    pub fn consume(self) -> (Stream, Option<HttpPersistentSettings>) {
        (self.stream, self.persistent_settings)
    }
}

/// This trait defines the interface of an HTTP application.
#[async_trait]
pub trait HttpServerApp {
    /// Similar to the [`ServerApp`], this function is called whenever a new HTTP session is established.
    ///
    /// After successful processing, [`ServerSession::finish()`] can be called to return an optionally reusable
    /// connection back to the service. The caller needs to make sure that the connection is in a reusable state
    /// i.e., no error or incomplete read or write headers or bodies. Otherwise a `None` should be returned.
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        // TODO: make this ShutdownWatch so that all task can await on this event
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream>;

    /// Provide options on how HTTP/2 connection should be established. This function will be called
    /// every time a new HTTP/2 **connection** needs to be established.
    ///
    /// A `None` means to use the built-in default options. See [`server::H2Options`] for more details.
    fn h2_options(&self) -> Option<server::H2Options> {
        None
    }

    /// Provide HTTP server options used to override default behavior. This function will be called
    /// every time a new connection is processed.
    ///
    /// A `None` means no server options will be applied.
    fn server_options(&self) -> Option<&HttpServerOptions> {
        None
    }

    async fn http_cleanup(&self) {}

    #[doc(hidden)]
    async fn process_custom_session(
        self: Arc<Self>,
        _stream: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        None
    }
}

#[async_trait]
impl<T> ServerApp for T
where
    T: HttpServerApp + Send + Sync + 'static,
{
    async fn process_new(
        self: &Arc<Self>,
        mut stream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let mut h2c = self.server_options().as_ref().map_or(false, |o| o.h2c);
        let custom = self
            .server_options()
            .as_ref()
            .map_or(false, |o| o.force_custom);

        // try to read h2 preface
        if h2c && !custom {
            let mut buf = [0u8; H2_PREFACE.len()];
            let peeked = stream
                .try_peek(&mut buf)
                .await
                .map_err(|e| {
                    // this error is normal when h1 reuse and close the connection
                    debug!("Read error while peeking h2c preface {e}");
                    e
                })
                .ok()?;
            // not all streams support peeking
            if peeked {
                // turn off h2c (use h1) if h2 preface doesn't exist
                h2c = buf == H2_PREFACE;
            }
        }
        if h2c || matches!(stream.selected_alpn_proto(), Some(ALPN::H2)) {
            // create a shared connection digest
            let digest = Arc::new(Digest {
                ssl_digest: stream.get_ssl_digest(),
                // TODO: log h2 handshake time
                timing_digest: stream.get_timing_digest(),
                proxy_digest: stream.get_proxy_digest(),
                socket_digest: stream.get_socket_digest(),
            });

            let h2_options = self.h2_options();
            let h2_conn = server::handshake(stream, h2_options).await;
            let mut h2_conn = match h2_conn {
                Err(e) => {
                    error!("H2 handshake error {e}");
                    return None;
                }
                Ok(c) => c,
            };

            let mut shutdown = shutdown.clone();
            loop {
                // this loop ends when the client decides to close the h2 conn
                // TODO: add a timeout?
                let h2_stream = tokio::select! {
                    _ = shutdown.changed() => {
                        h2_conn.graceful_shutdown();
                        let _ = poll_fn(|cx| h2_conn.poll_closed(cx))
                            .await.map_err(|e| error!("H2 error waiting for shutdown {e}"));
                        return None;
                    }
                    h2_stream = server::HttpSession::from_h2_conn(&mut h2_conn, digest.clone()) => h2_stream
                };
                let h2_stream = match h2_stream {
                    Err(e) => {
                        // It is common for the client to just disconnect TCP without properly
                        // closing H2. So we don't log the errors here
                        debug!("H2 error when accepting new stream {e}");
                        return None;
                    }
                    Ok(s) => s?, // None means the connection is ready to be closed
                };
                let app = self.clone();
                let shutdown = shutdown.clone();
                pingora_runtime::current_handle().spawn(async move {
                    // Note, `PersistentSettings` not currently relevant for h2
                    app.process_new_http(ServerSession::new_http2(h2_stream), &shutdown)
                        .await;
                });
            }
        } else if custom || matches!(stream.selected_alpn_proto(), Some(ALPN::Custom(_))) {
            return self.clone().process_custom_session(stream, shutdown).await;
        } else {
            // No ALPN or ALPN::H1 and h2c was not configured, fallback to HTTP/1.1
            let mut session = ServerSession::new_http1(stream);
            if *shutdown.borrow() {
                // stop downstream from reusing if this service is shutting down soon
                session.set_keepalive(None);
            } else {
                // default 60s
                session.set_keepalive(Some(60));
            }
            session.set_keepalive_reuses_remaining(
                self.server_options()
                    .and_then(|opts| opts.keepalive_request_limit),
            );

            let mut result = self.process_new_http(session, shutdown).await;
            while let Some((stream, persistent_settings)) = result.map(|r| r.consume()) {
                let mut session = ServerSession::new_http1(stream);
                if let Some(persistent_settings) = persistent_settings {
                    persistent_settings.apply_to_session(&mut session);
                }

                result = self.process_new_http(session, shutdown).await;
            }
        }
        None
    }

    async fn cleanup(&self) {
        self.http_cleanup().await;
    }
}
