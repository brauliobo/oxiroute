#[allow(clippy::wildcard_imports)]
use super::*;

/// The steps of an upstream attempt that do not depend on how the request arrived, shared by the Pingora hooks and
/// the fast lane.
impl HttpRequestContext {
    /// The peer of the next attempt: selects the endpoint (a retry on the same server, an affinity server or the
    /// pool's choice) the first time, and prepares the peer with the attempt's timeouts. `affinity` names a server
    /// the request is bound to; `host` is the `Host` the origin gets for the selected endpoint.
    pub(super) async fn next_peer(
        &mut self,
        affinity: impl FnOnce(&Self) -> Option<String>,
        host: impl FnOnce(
            &RuntimeEndpoint,
            &HttpUpstreamHost,
            Option<&Authority>,
        ) -> pingora::Result<Option<HeaderValue>>,
    ) -> pingora::Result<Box<HttpPeer>> {
        if self.has_attempted() {
            self.refresh_clock();
        }
        let mut now = self.now;
        let remaining = self.remaining_at(now, true)?;
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
                let wait = self.remaining_at(now, true)?;
                let (selected, waited) = promptly(
                    wait,
                    pool.select_endpoint(&self.attempted_upstreams, self.queue_tag),
                )
                .await
                .map_err(|_| Error::new_up(ErrorType::ReadTimedout))?;
                (selected?, waited)
            };
            if waited {
                self.now = Instant::now();
                now = self.now;
            }
            self.trace.queue_wait += now.saturating_duration_since(selection_started);
            self.trace.attempt_started = Some(now);
            let server: Arc<str> = selected.server_name().into();
            self.trace.servers.push(Arc::clone(&server));
            self.trace.stats = Some(Arc::clone(selected.server_stats()));
            if let Some(inflight) = &self.inflight {
                inflight.place_pool(pool.label().shared());
                inflight.place_server(server);
                selected.observe_queue_position(inflight.queue_cell());
                inflight.set_phase(Phase::Connecting, now);
            }
            self.selected_upstream_host = host(
                selected.endpoint(),
                &proxy_policy(self).upstream_host,
                self.authority.as_ref(),
            )?;
            self.attempted_upstreams
                .push(selected.server_name().to_owned());
            self.selected_observation = Some(selected.observation());
            self.selected = Some(selected);
        }
        let remaining = self.remaining_at(now, true)?;
        let route_policy = proxy_route(self).policy;
        let timeouts = proxy_policy(self).timeouts;
        let attempt = |explicit: Option<u64>, inherited: Duration| {
            explicit
                .map_or(inherited, Duration::from_millis)
                .min(remaining)
        };
        let response = pool.server_timeout(route_policy.read_timeout);
        let mut peer = self
            .selected
            .as_mut()
            .expect("selected endpoint initialized")
            .prepare_peer_with_timeouts(
                pool,
                now,
                attempt(
                    timeouts.connect_ms,
                    pool.connect_timeout(route_policy.connect_timeout),
                ),
                attempt(timeouts.header_ms, response),
                pool.server_timeout(route_policy.write_timeout)
                    .min(remaining),
            )
            .await?;
        peer.options.body_read_timeout = Some(attempt(timeouts.idle_ms, response));
        Ok(Box::new(peer))
    }

    /// An attempt could not connect: records the failure and decides, with the retry policy, whether to go on.
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
        self.remaining_at(now, true)?;
        self.enter_phase(Phase::AwaitingHeaders, now);
        self.trace.connected_at = Some(now);
        self.trace.connect = self
            .trace
            .attempt_started
            .map(|started| now.saturating_duration_since(started));
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
    pub(super) fn arm_evacuation_for_lane(&self) -> UpstreamCancellation {
        let Some(evacuation) = self.evacuation() else {
            return Box::pin(std::future::pending());
        };
        evacuation.begin_attempt(self.evacuation_pin);
        evacuation.cancellation()
    }

    /// An attempt ended in `error`: what `error_while_proxy` settles for a route that does not retry.
    pub(super) fn attempt_failed_for_lane(&mut self, mut error: Box<Error>) -> Box<Error> {
        if let Some(cancelled) = self.cancelled_by_operator() {
            return cancelled;
        }
        if let Some(failure) = passive_failure_for_error(&error) {
            self.record_passive_failure(failure);
        }
        self.release_lease();
        error.set_retry(false);
        self.settle_attempt(&error, false);
        error
    }
}
