#[allow(clippy::wildcard_imports)]
use super::*;
use crate::{http::action::RoutePolicyPlan, upstream_peer::ReusePeer};
use oxiroute_config::HttpProxyTimeouts;

/// What an attempt allows each of its steps once its server is selected.
#[derive(Clone, Copy)]
pub(super) struct AttemptTimeouts {
    now: Instant,
    connect: Duration,
    read: Duration,
    write: Duration,
    body_read: Duration,
}

/// What the attempts of a route allow each step before the time the request has left is taken into account. It
/// follows from the route's policy and its pool alone, so a route that serves many requests works it out once.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AttemptBase {
    connect: Duration,
    response: Duration,
    write: Duration,
    explicit: HttpProxyTimeouts,
}

impl AttemptBase {
    pub(crate) fn of(
        route: RoutePolicyPlan,
        explicit: HttpProxyTimeouts,
        pool: &UpstreamPlan,
    ) -> Self {
        Self {
            connect: pool.connect_timeout(route.connect_timeout),
            response: pool.server_timeout(route.read_timeout),
            write: pool.server_timeout(route.write_timeout),
            explicit,
        }
    }

    fn within(&self, now: Instant, remaining: Duration) -> AttemptTimeouts {
        let attempt = |explicit: Option<u64>, inherited: Duration| {
            explicit
                .map_or(inherited, Duration::from_millis)
                .min(remaining)
        };
        AttemptTimeouts {
            now,
            connect: attempt(self.explicit.connect_ms, self.connect),
            read: attempt(self.explicit.header_ms, self.response),
            write: self.write.min(remaining),
            body_read: attempt(self.explicit.idle_ms, self.response),
        }
    }
}

impl AttemptTimeouts {
    /// Applies the attempt's read and write limits to the session of its connection.
    pub(super) fn apply(&self, session: &mut pingora::protocols::http::v1::client::HttpSession) {
        self.apply_write(session);
        self.apply_reads(session);
    }

    /// The write limit alone: the fast lane keeps the read limits itself (`lane::timing`).
    pub(super) fn apply_write(
        &self,
        session: &mut pingora::protocols::http::v1::client::HttpSession,
    ) {
        session.write_timeout = Some(self.write);
    }

    /// The read limits alone, for a response the session's own parser reads.
    pub(super) fn apply_reads(
        &self,
        session: &mut pingora::protocols::http::v1::client::HttpSession,
    ) {
        session.read_timeout = Some(self.read);
        session.body_read_timeout = Some(self.body_read);
    }

    /// How long a connection may take to be established.
    pub(super) fn connect(&self) -> Duration {
        self.connect
    }

    /// How long a write may take.
    pub(super) fn write(&self) -> Duration {
        self.write
    }

    /// How long the response head may take.
    pub(super) fn read(&self) -> Duration {
        self.read
    }

    /// How long the response body may be silent.
    pub(super) fn body_read(&self) -> Duration {
        self.body_read
    }
}

/// The steps of an upstream attempt that do not depend on how the request arrived, shared by the Pingora hooks and
/// the fast lane.
impl HttpRequestContext {
    /// The peer of the next attempt: selects the endpoint, then prepares the peer with the attempt's timeouts.
    /// `affinity` names a server the request is bound to; `host` is the `Host` the origin gets for the selected
    /// endpoint.
    pub(super) async fn next_peer(
        &mut self,
        affinity: impl FnOnce(&Self) -> Option<String>,
        host: impl FnOnce(
            &RuntimeEndpoint,
            &HttpUpstreamHost,
            Option<&Authority>,
        ) -> pingora::Result<Option<HeaderValue>>,
    ) -> pingora::Result<Box<HttpPeer>> {
        let timeouts = self.next_attempt(affinity, host).await?;
        self.prepare_peer(timeouts).await
    }

    /// The idle-pool peer of the selected server, when the attempt can take a connection without building a peer.
    pub(super) fn reuse_peer(&self) -> Option<ReusePeer> {
        let reuses = self.pool.as_ref()?.connection_reuse()
            != oxiroute_config::UpstreamConnectionReuse::Never;
        self.selected.as_ref().filter(|_| reuses)?.reuse_peer()
    }

    /// Selects the endpoint (a retry on the same server, an affinity server or the pool's choice) the first time,
    /// and works out the attempt's timeouts.
    pub(super) async fn next_attempt(
        &mut self,
        affinity: impl FnOnce(&Self) -> Option<String>,
        host: impl FnOnce(
            &RuntimeEndpoint,
            &HttpUpstreamHost,
            Option<&Authority>,
        ) -> pingora::Result<Option<HeaderValue>>,
    ) -> pingora::Result<AttemptTimeouts> {
        if self.has_attempted() {
            self.refresh_clock();
        }
        let mut now = self.now;
        let mut remaining = self.remaining_at(now, true)?;
        let Some(pool) = &self.pool else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        if self.selected.is_none() {
            if self.retry_delay_pending {
                let delay = proxy_policy(self).next_retry_delay();
                if delay > remaining {
                    return Err(Error::new_up(ErrorType::ReadTimedout));
                }
                self.enter_phase(Phase::RetryWait, now);
                tokio::time::sleep(delay).await;
                self.retry_delay_pending = false;
                self.now = Instant::now();
                now = self.now;
                remaining = self.remaining_at(now, true)?;
            }
            self.enter_phase(Phase::Queued, now);
            let selection_started = now;
            let (selected, waited) = if let Some(server) = self.retry_server.take() {
                (
                    pool.select_server_endpoint(&server, self.queue_tag).await?,
                    true,
                )
            } else if let Some(server) = affinity(self) {
                (
                    pool.select_server_endpoint(&server, self.queue_tag).await?,
                    true,
                )
            } else {
                let (selected, waited) = promptly(
                    remaining,
                    pool.select_endpoint_at(
                        &self.attempted_upstreams,
                        self.queue_tag,
                        crate::clock::wall_ms(now),
                    ),
                )
                .await
                .map_err(|_| Error::new_up(ErrorType::ReadTimedout))?;
                (selected?, waited)
            };
            if waited {
                self.now = Instant::now();
                now = self.now;
                remaining = self.remaining_at(now, true)?;
                self.trace.queue_wait += now.saturating_duration_since(selection_started);
            }
            self.adopt_selection(selected, now, host)?;
        }
        Ok(self.attempt_timeouts(now, remaining))
    }

    /// The selected server becomes the one the attempt goes to: what the access line, the status page and the server's
    /// counters say of the attempt, and the host the origin gets.
    fn adopt_selection(
        &mut self,
        selected: SelectedEndpoint,
        now: Instant,
        host: impl FnOnce(
            &RuntimeEndpoint,
            &HttpUpstreamHost,
            Option<&Authority>,
        ) -> pingora::Result<Option<HeaderValue>>,
    ) -> pingora::Result<()> {
        let pool = self.pool.as_ref().expect("a pool was selected");
        self.trace.attempt_started = Some(now);
        let server = selected.server_label().clone();
        self.trace.servers.push(server.clone());
        self.trace.stats = Some(Arc::clone(selected.server_stats()));
        if let Some(inflight) = &self.inflight {
            inflight.place_attempt(pool.label(), server.shared());
            selected.observe_queue_position(|| inflight.queue_cell());
            inflight.set_phase(Phase::Connecting, now);
        }
        self.selected_upstream_host = host(
            selected.endpoint(),
            &proxy_policy(self).upstream_host,
            self.authority.as_ref(),
        )?;
        self.attempted_upstreams.push(server);
        self.selected_observation = Some(selected.observation());
        self.selected = Some(selected);
        Ok(())
    }

    fn attempt_timeouts(&self, now: Instant, remaining: Duration) -> AttemptTimeouts {
        let pool = self.pool.as_ref().expect("a pool was selected");
        AttemptBase::of(proxy_route(self).policy, proxy_policy(self).timeouts, pool)
            .within(now, remaining)
    }

    /// [`Self::next_attempt`] for a caller that cannot wait: `None` when the attempt would have to (the retry delay
    /// has not passed, or the pool has no server to give at once), with the attempt not begun. The routes the epoll
    /// data plane serves never wait: their pools answer a selection at once and their retries have no delay.
    pub(super) fn next_attempt_now(
        &mut self,
        base: &AttemptBase,
        host: impl FnOnce(
            &RuntimeEndpoint,
            &HttpUpstreamHost,
            Option<&Authority>,
        ) -> pingora::Result<Option<HeaderValue>>,
    ) -> pingora::Result<Option<AttemptTimeouts>> {
        if self.has_attempted() {
            self.refresh_clock();
        }
        let now = self.now;
        let remaining = self.remaining_at(now, true)?;
        if self.selected.is_none() {
            if self.retry_delay_pending && !proxy_policy(self).next_retry_delay().is_zero() {
                return Ok(None);
            }
            self.retry_delay_pending = false;
            let pool = self
                .pool
                .as_ref()
                .ok_or_else(|| Error::new_in(ErrorType::InternalError))?;
            if !pool.selects_at_once() {
                return Ok(None);
            }
            let selected = match self.retry_server.take() {
                Some(server) => pool.select_server_endpoint_at_once(&server),
                None => pool
                    .select_endpoint_at_once(&self.attempted_upstreams, crate::clock::wall_ms(now)),
            };
            self.enter_phase(Phase::Queued, now);
            self.adopt_selection(selected?, now, host)?;
        }
        Ok(Some(base.within(now, remaining)))
    }

    /// The lease of the selected server, which the connection of the attempt owns from now on.
    pub(super) fn take_lease(&mut self) -> Option<crate::EndpointLease> {
        self.note_queue_wait();
        self.selected.take().map(SelectedEndpoint::into_lease)
    }

    /// The idle-pool peer of the selected server, whether or not the pool reuses connections.
    pub(super) fn selected_peer(&self) -> Option<ReusePeer> {
        self.selected.as_ref()?.reuse_peer()
    }

    /// The request goes to another reader of the connection before any server was contacted: what it counted as an
    /// operation is the other reader's to count.
    pub(super) fn abandon(&mut self) {
        self.operation_result = Some(HttpOperationResult::Cancelled);
    }

    /// The peer for connecting, from the endpoint [`Self::next_attempt`] selected.
    pub(super) async fn prepare_peer(
        &mut self,
        timeouts: AttemptTimeouts,
    ) -> pingora::Result<Box<HttpPeer>> {
        let Some(pool) = &self.pool else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        let mut peer = self
            .selected
            .as_mut()
            .expect("selected endpoint initialized")
            .prepare_peer_with_timeouts(
                pool,
                timeouts.now,
                timeouts.connect,
                timeouts.read,
                timeouts.write,
            )
            .await?;
        peer.options.body_read_timeout = Some(timeouts.body_read);
        Ok(Box::new(peer))
    }

    /// An attempt could not connect: records the failure and decides, with the retry policy, whether to go on.
    #[allow(clippy::unnecessary_box_returns)]
    pub(super) fn connect_failed(&mut self, mut error: Box<Error>) -> Box<Error> {
        if let Some(cancelled) = self.cancelled_by_operator() {
            return cancelled;
        }
        let has_address_fallback = self
            .selected
            .as_ref()
            .is_some_and(SelectedEndpoint::has_address_fallback);
        if !has_address_fallback {
            if let Some(failure) = passive_failure_for_error(&error) {
                self.record_passive_failure(failure);
            }
            self.release_lease();
        }
        let policy = proxy_policy(self);
        let has_budget = self.attempt_count() <= usize::from(policy.max_retries);
        let retry_target = retry_target_for(self, false);
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => self.attempted_upstreams.last().is_some(),
            HttpRetryTarget::NextServer => self
                .pool
                .as_ref()
                .is_some_and(|pool| pool.has_unattempted(&self.attempted_upstreams)),
        };
        let trigger = connect_retry_trigger(&error);
        let route_retry_allowed = !has_address_fallback
            && self.connection_retryable
            && trigger.is_some_and(|trigger| policy.retries_on(trigger))
            && has_budget
            && target_available
            && self.remaining(true).is_ok()
            && self.spend_retry_budget();
        let retry = should_retry_connection(has_address_fallback, route_retry_allowed);
        error.set_retry(retry);
        if !has_address_fallback {
            self.settle_attempt(&error, retry);
        }
        if retry {
            if !has_address_fallback {
                if retry_target == HttpRetryTarget::SameServer {
                    self.retry_server = self.attempted_upstreams.last().cloned();
                }
                self.retry_delay_pending = retry_target == HttpRetryTarget::SameServer;
            }
            self.listener.record_retry_attempt();
        }
        error
    }

    /// The connection of the attempt is established, and from now on the lease belongs to the socket.
    pub(super) fn connected(
        &mut self,
        reused: bool,
        digest: Option<&Digest>,
    ) -> pingora::Result<()> {
        let queued = self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.queue_position() > 0);
        let now = if reused && !queued {
            self.now
        } else {
            self.refresh_clock()
        };
        self.ensure_open_at(now, true)?;
        self.enter_phase(Phase::AwaitingHeaders, now);
        self.trace.connected_at = Some(now);
        self.trace.connect = self
            .trace
            .attempt_started
            .map(|started| crate::clock::since(now, started));
        let result =
            validate_tls_connection(self.pool.as_deref().and_then(UpstreamPlan::tls), digest);
        if result.is_err() {
            if let Err(error) = &result
                && let Some(failure) = passive_failure_for_error(error)
            {
                self.record_passive_failure(failure);
            }
            self.release_lease();
        } else {
            // The Pingora socket digest now owns the lease until the physical connection closes.
            self.detach_lease();
        }
        result
    }
}

/// What the fast lane needs of an attempt: its request has no body, so it can always move, and the lane's routes
/// never retry, so a failed attempt is final unless an operator moved it.
impl HttpRequestContext {
    pub(super) fn arm_evacuation_for_lane(&self) -> crate::http::evacuation::Cancelled {
        let Some(inflight) = self.inflight.as_ref() else {
            return crate::http::evacuation::Cancelled::never();
        };
        inflight.evacuation().begin_attempt(self.evacuation_pin);
        inflight.cancelled()
    }
}

/// What the exchange has done by the time an attempt fails, which decides whether the request may be sent again.
#[derive(Clone, Copy)]
pub(super) struct Exchanged {
    /// The request body, if any, can be sent again.
    pub(super) body_replayable: bool,
    /// The client has seen no final response.
    pub(super) response_retryable: bool,
}

impl Exchanged {
    /// An attempt that had sent nothing a client could notice: no body read, no response begun.
    pub(super) const UNANSWERED: Self = Self {
        body_replayable: true,
        response_retryable: true,
    };
}

impl HttpRequestContext {
    /// An attempt failed after it connected, with `error`: records what the failure says of the server, releases the
    /// lease and decides, with the retry policy, whether the request goes on. What `error_while_proxy` settles for
    /// both paths.
    #[allow(clippy::unnecessary_box_returns)]
    pub(super) fn attempt_failed(
        &mut self,
        mut error: Box<Error>,
        exchanged: Exchanged,
    ) -> Box<Error> {
        let prepaid = std::mem::take(&mut self.retry_prepaid);
        if let Some(cancelled) = self.cancelled_by_operator() {
            return cancelled;
        }
        let rescue_only = is_rescue_only(&error, proxy_policy(self));
        if let Some(failure) = passive_failure_for_error(&error).filter(|_| !rescue_only) {
            self.record_passive_failure(failure);
        }
        self.release_lease();
        let policy = proxy_policy(self);
        let refused = is_pool_unavailable(&error);
        let has_budget = self.attempt_count() <= usize::from(policy.max_retries);
        let retry_target = retry_target_for(self, refused);
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => self.attempted_upstreams.last().is_some(),
            HttpRetryTarget::NextServer => self
                .pool
                .as_ref()
                .is_some_and(|pool| pool.has_unattempted(&self.attempted_upstreams)),
        };
        let eligible = if refused {
            self.connection_retryable
        } else {
            self.replay_retryable
                && retryable_upstream_error(&error, exchanged.response_retryable, policy)
        };
        let retry = eligible
            && exchanged.body_replayable
            && exchanged.response_retryable
            && has_budget
            && target_available
            && self.remaining(true).is_ok()
            && (prepaid || self.spend_retry_budget());
        error.set_retry(retry);
        self.settle_attempt(&error, retry);
        if retry {
            if retry_target == HttpRetryTarget::SameServer {
                self.retry_server = self.attempted_upstreams.last().cloned();
            }
            self.retry_delay_pending = retry_target == HttpRetryTarget::SameServer;
            self.listener.record_retry_attempt();
        }
        error
    }
}
