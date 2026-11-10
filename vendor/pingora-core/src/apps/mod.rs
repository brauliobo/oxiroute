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

use crate::server::ShutdownWatch;
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use log::{debug, error};
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::protocols::http::v2::server;
use crate::protocols::http::{ReusableHttpStream, ServerSession};
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
    /// Threads blocked in `AcceptGateClose::wait`; a wakeup costs a syscall, so nobody notifies without one.
    waiters: usize,
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
                    waiters: 0,
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

    fn notify_quiesced(&self, state: &AcceptGateInnerState) {
        if state.waiters > 0 {
            self.inner.quiesced.notify_all();
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
        state.waiters += 1;
        let quiesced = loop {
            let quiesced = state.claims == 0
                && state
                    .participants
                    .values()
                    .all(|acknowledged| *acknowledged >= self.epoch);
            let now = Instant::now();
            if quiesced || now >= deadline {
                break quiesced;
            }
            let (next, timeout) = self
                .gate
                .inner
                .quiesced
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() {
                break state.claims == 0
                    && state
                        .participants
                        .values()
                        .all(|acknowledged| *acknowledged >= self.epoch);
            }
        };
        state.waiters -= 1;
        quiesced
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
        self.gate.notify_quiesced(&state);
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
        self.gate.notify_quiesced(&state);
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
        self.gate.notify_quiesced(&state);
    }
}

#[cfg(test)]
mod h2_shutdown_tests;

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
#[derive(Clone, Debug, Default)]
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

    /// If set, close a downstream HTTP/2 connection that has been idle
    /// for this duration.
    ///
    /// Default: `None`
    pub h2_idle_timeout: Option<Duration>,
}

/// Settings persisted across HTTP/1.x keepalive requests on the same downstream connection.
///
/// In addition to framework-managed keepalive parameters, this struct can carry an optional
/// user-defined context via [`set_user_context`](Self::set_user_context). The proxy layer
/// populates this through `ProxyHttp::persist_connection_context`
/// and delivers it to the next request through `ProxyHttp::on_connection_reuse`.
///
/// Also carries pipelined-prefix bytes when the caller has opted into HTTP/1.1
/// pipelining on the previous session. See
/// [`Self::set_pipelined_prefix`] and the
/// [`HttpSession::set_pipelining_enabled`](crate::protocols::http::v1::server::HttpSession::set_pipelining_enabled)
/// docs for the RFC 9112 §9.3.2 semantics.
#[derive(Debug)]
pub struct HttpPersistentSettings {
    keepalive_timeout: Option<u64>,
    keepalive_reuses_remaining: Option<u32>,
    /// User-defined context to carry to the next request on this connection.
    user_context: Option<Box<dyn Any + Send + Sync>>,
    /// Bytes read past the end of the previous request's body, to be parsed
    /// as the next pipelined request on the reused connection.
    pipelined_prefix: Option<BytesMut>,
    /// Whether HTTP/1.1 pipelining was enabled on the previous session;
    /// propagates to the next session so the proxy-level opt-in sticks
    /// across keepalive reuses without the adopter having to re-enable
    /// it on every request.
    pipelining_enabled: bool,
}

impl HttpPersistentSettings {
    pub fn for_session(session: &ServerSession) -> Self {
        HttpPersistentSettings {
            keepalive_timeout: session.get_keepalive(),
            keepalive_reuses_remaining: session.get_keepalive_reuses_remaining(),
            user_context: None,
            pipelined_prefix: None,
            pipelining_enabled: session.pipelining_enabled(),
        }
    }

    /// Set a user-defined context to be carried to the next request on this connection.
    pub fn set_user_context(&mut self, ctx: Box<dyn Any + Send + Sync>) {
        self.user_context = Some(ctx);
    }

    /// Take the user-defined context, if any.
    pub fn take_user_context(&mut self) -> Option<Box<dyn Any + Send + Sync>> {
        self.user_context.take()
    }

    /// Set pipelined-prefix bytes to be fed to the next session on this
    /// connection. Called by the proxy layer when HTTP/1.1 pipelining is
    /// enabled on the current session and overread bytes were present at
    /// reuse time.
    pub fn set_pipelined_prefix(&mut self, prefix: BytesMut) {
        self.pipelined_prefix = Some(prefix);
    }

    pub fn apply_to_session(self, session: &mut ServerSession) {
        let Self {
            keepalive_timeout,
            mut keepalive_reuses_remaining,
            user_context,
            pipelined_prefix,
            pipelining_enabled,
        } = self;

        // Reduce the number of times the connection for this session can be
        // reused by one. A session with reuse count of zero won't be reused
        if let Some(reuses) = keepalive_reuses_remaining.as_mut() {
            *reuses = reuses.saturating_sub(1);
        }

        session.set_keepalive(keepalive_timeout);
        session.set_keepalive_reuses_remaining(keepalive_reuses_remaining);
        session.mark_reused_connection();

        // Carry user context into the session for the proxy layer to consume
        session.set_connection_user_context(user_context);

        // Replay pipelining opt-in so it stays on across keepalive reuses.
        session.set_pipelining_enabled(pipelining_enabled);

        // Feed any pipelined prefix bytes to the new session's request parser
        // so they are treated as the start of the next request.
        if let Some(prefix) = pipelined_prefix {
            session.set_pipelined_prefix(prefix);
        }
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

    /// Build a reusable HTTP stream from a finished session, preserving any
    /// pipelined prefix bytes in the persistent settings for the next request.
    pub fn from_reusable_stream(
        reusable: ReusableHttpStream,
        mut persistent_settings: HttpPersistentSettings,
    ) -> Self {
        let (stream, pipelined_prefix) = reusable.into_parts();
        if let Some(prefix) = pipelined_prefix {
            persistent_settings.set_pipelined_prefix(prefix);
        }
        Self::new(stream, Some(persistent_settings))
    }

    pub fn consume(self) -> (Stream, Option<HttpPersistentSettings>) {
        (self.stream, self.persistent_settings)
    }
}

/// What an HTTP/1 connection carries from one request to the next, whichever side serves them: the keepalive
/// setting, the number of requests it may still serve and whether it served one already.
#[derive(Debug)]
pub struct Http1Connection {
    keepalive_timeout: Option<u64>,
    reuses_remaining: Option<u32>,
    reused: bool,
    /// What the previous session handed over beyond the keepalive state: the user context, the pipelining
    /// opt-in and the bytes it read past its request.
    carried: Option<HttpPersistentSettings>,
}

impl Http1Connection {
    fn first(shutting_down: bool, request_limit: Option<u32>) -> Self {
        Self {
            keepalive_timeout: (!shutting_down).then_some(60),
            reuses_remaining: request_limit,
            reused: false,
            carried: None,
        }
    }

    fn after(settings: Option<HttpPersistentSettings>) -> Self {
        let (keepalive_timeout, reuses_remaining) = settings.as_ref().map_or((None, None), |settings| {
            (settings.keepalive_timeout, settings.keepalive_reuses_remaining)
        });
        Self {
            keepalive_timeout,
            reuses_remaining,
            reused: true,
            carried: settings,
        }
    }

    /// Whether the previous session read bytes of the next request already, which only a session parses.
    pub fn has_buffered_request(&self) -> bool {
        self.carried
            .as_ref()
            .and_then(|settings| settings.pipelined_prefix.as_ref())
            .is_some_and(|prefix| !prefix.is_empty())
    }

    /// Whether the connection served a request already.
    pub fn is_reused(&self) -> bool {
        self.reused
    }

    /// The setting of a session for the next request, as [`HttpPersistentSettings::apply_to_session`] gives it.
    pub fn apply_to_session(&mut self, session: &mut ServerSession) {
        session.set_keepalive(self.keepalive_timeout);
        session.set_keepalive_reuses_remaining(self.reuses_remaining_for_request());
        if self.reused {
            session.mark_reused_connection();
        }
        if let Some(carried) = self.carried.take() {
            session.set_connection_user_context(carried.user_context);
            session.set_pipelining_enabled(carried.pipelining_enabled);
            if let Some(prefix) = carried.pipelined_prefix {
                session.set_pipelined_prefix(prefix);
            }
        }
    }

    fn reuses_remaining_for_request(&self) -> Option<u32> {
        match self.reuses_remaining {
            Some(reuses) if self.reused => Some(reuses.saturating_sub(1)),
            other => other,
        }
    }

    /// Starts a request the caller serves itself: the request counts against the connection's limit.
    pub fn begin_request(&mut self) {
        self.reuses_remaining = self.reuses_remaining_for_request();
    }

    /// What `respect_keepalive` does for an HTTP/1.1 request: keepalive stays on unless the client asked to close.
    pub fn respect_request(&mut self, client_closes: bool) {
        self.keepalive_timeout = (!client_closes).then_some(0);
    }

    /// Ends keepalive, as a shutting-down service does for the request it has just read.
    pub fn disable_keepalive(&mut self) {
        self.keepalive_timeout = None;
    }

    /// Whether the connection serves another request after the current one.
    pub fn will_keepalive(&self) -> bool {
        self.keepalive_timeout.is_some() && self.reuses_remaining != Some(0)
    }

    /// The caller finished a request it served itself.
    pub fn end_request(&mut self) {
        self.reused = true;
    }
}

/// A connection-level front of the HTTP/1 loop: it sees the stream before any session exists and serves the
/// requests it can by itself, handing the connection to the application's session for the rest.
#[async_trait]
pub trait Http1Front: Send + Sync {
    /// Serves requests of `stream` until one needs the application's session or the connection ends.
    async fn serve(
        &self,
        stream: Stream,
        connection: &mut Http1Connection,
        shutdown: &ShutdownWatch,
    ) -> Http1FrontOutcome;

    /// The service stops: ends the waits of connections that have not served a request.
    fn cleanup(&self) {}
}

/// How a [`Http1Front`] left a connection.
pub enum Http1FrontOutcome {
    Closed,
    /// The next request is the session's: `preloaded` holds the bytes of it the front already read.
    Handoff { stream: Stream, preloaded: Bytes },
}

/// This trait defines the interface of an HTTP application.
#[async_trait]
pub trait HttpServerApp {
    /// Similar to the [`ServerApp`], this function is called whenever a new HTTP session is established.
    ///
    /// After successful processing, [`ServerSession::finish()`] can be
    /// called to return an optionally reusable connection back to the service.
    /// The caller needs to make sure that the connection is in a reusable state
    /// i.e., no error or incomplete read or write headers or bodies. Otherwise
    /// a `None` should be returned.
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        // TODO: make this ShutdownWatch so that all task can await on this event
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream>;

    /// [`Self::process_new_http()`] for the request loop of an HTTP/1 connection. The connection awaits the
    /// returned future where it is, whereas the boxed future of `process_new_http` moves the whole request
    /// state machine (8 KiB and more for a proxy) to the heap for every request. The default calls
    /// `process_new_http`; an application whose request future is large implements both.
    fn process_new_http_inline(
        self: &Arc<Self>,
        session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> impl std::future::Future<Output = Option<ReusedHttpStream>> + Send
    where
        Self: Send + Sync,
    {
        async move { self.process_new_http(session, shutdown).await }
    }

    /// Provide options on how HTTP/2 connection should be established. This function will be called
    /// every time a new HTTP/2 **connection** needs to be established.
    ///
    /// A `None` means to use the built-in default options. See [`server::H2Options`] for more details.
    /// The connection-level front of HTTP/1 connections, if the application has one.
    fn http1_front(&self) -> Option<&dyn Http1Front> {
        None
    }

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

        // h2c is for cleartext connections; on TLS, ALPN handles protocol negotiation.
        // Otherwise, h2c stays true on TLS streams, forcing HTTP/1.1 clients into HTTP/2
        if stream.get_ssl_digest().is_some() {
            h2c = false;
        }
        // try to read h2 preface
        else if h2c && !custom {
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
            let h2_conn = match server::handshake(stream, h2_options).await {
                Err(e) => {
                    error!("H2 handshake error {e}");
                    return None;
                }
                Ok(c) => c,
            };

            // The accept-loop body — including the graceful-shutdown state
            // machine — lives in `server::accept_downstream_sessions` so that
            // the same code path is exercised by tests in `protocols::http::v2`.
            let app = self.clone();
            let shutdown_for_session = shutdown.clone();
            let h2_idle_timeout = self.server_options().and_then(|o| o.h2_idle_timeout);
            server::accept_downstream_sessions(
                h2_conn,
                digest,
                shutdown.clone(),
                h2_idle_timeout,
                |h2_stream, guard| {
                    let app = app.clone();
                    let shutdown = shutdown_for_session.clone();
                    pingora_runtime::current_handle().spawn(async move {
                        // hold `guard` for the session's lifetime so the accept
                        // loop's idle timeout sees this connection as busy.
                        let _guard = guard;
                        // Note, `PersistentSettings` not currently relevant for h2
                        app.process_new_http(ServerSession::new_http2(h2_stream), &shutdown)
                            .await;
                    });
                },
            )
            .await;
        } else if custom || matches!(stream.selected_alpn_proto(), Some(ALPN::Custom(_))) {
            return self.clone().process_custom_session(stream, shutdown).await;
        } else {
            // No ALPN or ALPN::H1 and h2c was not configured, fallback to HTTP/1.1
            let limit = self
                .server_options()
                .and_then(|opts| opts.keepalive_request_limit);
            let mut connection = Http1Connection::first(*shutdown.borrow(), limit);
            let front = self.http1_front();
            loop {
                let mut preloaded = Bytes::new();
                if let Some(front) = front.filter(|_| !connection.has_buffered_request()) {
                    match front.serve(stream, &mut connection, shutdown).await {
                        Http1FrontOutcome::Closed => return None,
                        Http1FrontOutcome::Handoff {
                            stream: handed,
                            preloaded: bytes,
                        } => {
                            stream = handed;
                            preloaded = bytes;
                        }
                    }
                }
                let mut session = ServerSession::new_http1_preloaded(stream, preloaded);
                connection.apply_to_session(&mut session);
                let Some((next, settings)) = self
                    .process_new_http_inline(session, shutdown)
                    .await
                    .map(ReusedHttpStream::consume)
                else {
                    return None;
                };
                stream = next;
                connection = Http1Connection::after(settings);
            }
        }
        None
    }

    async fn cleanup(&self) {
        if let Some(front) = self.http1_front() {
            front.cleanup();
        }
        self.http_cleanup().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_test::io::Builder;

    #[test]
    fn test_persistent_settings_user_context_roundtrip() {
        // Create a mock H1 session
        let mock_io = Builder::new().build();
        let mut session = ServerSession::new_http1(Box::new(mock_io));
        session.set_keepalive(Some(60));

        // Snapshot settings (no user context yet)
        let mut settings = HttpPersistentSettings::for_session(&session);
        assert!(settings.take_user_context().is_none());

        // Set user context
        settings.set_user_context(Box::new(123u64));

        // Apply to a fresh session -- user context should transfer
        let mock_io2 = Builder::new().build();
        let mut session2 = ServerSession::new_http1(Box::new(mock_io2));
        settings.apply_to_session(&mut session2);

        // The user context should now be on the session
        let ctx = session2.take_connection_user_context();
        assert!(ctx.is_some());
        let val = ctx.unwrap().downcast::<u64>().unwrap();
        assert_eq!(*val, 123u64);

        // Keepalive should also have been applied
        assert_eq!(session2.get_keepalive(), Some(60));
    }

    #[test]
    fn test_persistent_settings_no_user_context_by_default() {
        let mock_io = Builder::new().build();
        let mut session = ServerSession::new_http1(Box::new(mock_io));
        session.set_keepalive(Some(30));

        let settings = HttpPersistentSettings::for_session(&session);

        let mock_io2 = Builder::new().build();
        let mut session2 = ServerSession::new_http1(Box::new(mock_io2));
        settings.apply_to_session(&mut session2);

        // No user context should be present
        assert!(session2.take_connection_user_context().is_none());
        // Keepalive should still work
        assert_eq!(session2.get_keepalive(), Some(30));
    }
}
