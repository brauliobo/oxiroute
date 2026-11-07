#[allow(clippy::wildcard_imports)]
use super::*;

impl EndpointPool {
    pub(crate) fn acquire_compiled(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        startup: Option<HealthStartup>,
        queue_timeout: Option<Duration>,
        passive_policy: PassiveFailurePolicy,
        resilience: PoolResiliencePolicy,
        blueprint: &PoolConstructionBlueprint,
    ) -> Self {
        let endpoints = servers
            .into_iter()
            .zip(blueprint.weights.iter().copied())
            .map(|(server, weight)| Arc::new(PoolEndpoint::new(server, startup, weight)))
            .collect::<Arc<[_]>>();
        let queue = Arc::new(PoolQueue::new(queue_timeout.is_some()));
        let health = Arc::new(PoolHealthState::new(
            name.clone(),
            Arc::clone(&endpoints),
            Arc::clone(&queue),
            passive_policy,
            resilience,
        ));
        Self {
            algorithm: blueprint.algorithm.clone(),
            name,
            endpoints,
            weighted_schedule: blueprint.weighted_schedule.clone(),
            health,
            selection: Mutex::new(SelectionState::default()),
            queue,
            queue_timeout,
            resilience,
            unavailable_selections: AtomicU64::new(0),
            fail_open_selections: AtomicU64::new(0),
        }
    }

    /// Creates an unchecked round-robin pool from socket addresses.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an address cannot be represented by the runtime.
    pub fn new(endpoints: impl IntoIterator<Item = SocketAddr>) -> Result<Self, PoolError> {
        Self::from_endpoints(
            endpoints.into_iter().map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
        )
    }

    /// Creates an unchecked pool using the configured balancing algorithm.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an endpoint cannot be represented by the runtime.
    pub fn from_endpoints(
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
    ) -> Result<Self, PoolError> {
        Self::new_named(String::new(), endpoints, algorithm, false)
    }

    /// Creates an unchecked pool with an immutable passive failure policy.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an endpoint cannot be represented by the runtime.
    pub fn from_endpoints_with_policy(
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
        passive_policy: PassiveFailurePolicy,
    ) -> Result<Self, PoolError> {
        Self::new_named_servers_with_policy(
            String::new(),
            endpoints
                .into_iter()
                .enumerate()
                .map(|(index, endpoint)| RuntimeServer {
                    name: index.to_string(),
                    endpoint,
                    max_connections: None,
                    pinned_addresses: None,
                    protected_addresses: Arc::from([]),
                }),
            algorithm,
            None,
            None,
            passive_policy,
        )
    }

    pub(crate) fn new_named(
        name: String,
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
        checked: bool,
    ) -> Result<Self, PoolError> {
        let servers = endpoints
            .into_iter()
            .enumerate()
            .map(|(index, endpoint)| RuntimeServer {
                name: index.to_string(),
                endpoint,
                max_connections: None,
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            });
        Self::new_named_servers(
            name,
            servers,
            algorithm,
            checked.then_some(HealthStartup::Checking),
            None,
        )
    }

    pub(crate) fn new_named_servers(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
    ) -> Result<Self, PoolError> {
        Self::new_named_servers_with_policy(
            name,
            servers,
            algorithm,
            startup,
            queue_timeout,
            PassiveFailurePolicy::default(),
        )
    }

    pub(crate) fn new_named_servers_with_policy(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
        passive_policy: PassiveFailurePolicy,
    ) -> Result<Self, PoolError> {
        Self::new_named_servers_resilient(
            name,
            servers,
            algorithm,
            startup,
            queue_timeout,
            passive_policy,
            PoolResiliencePolicy::default(),
        )
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
    pub(crate) fn new_named_servers_resilient(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
        passive_policy: PassiveFailurePolicy,
        resilience: PoolResiliencePolicy,
    ) -> Result<Self, PoolError> {
        passive_policy.validate()?;
        let servers = servers.into_iter().collect::<Vec<_>>();
        for server in &servers {
            server.endpoint.preflight()?;
        }
        let blueprint = PoolConstructionBlueprint::compile(&algorithm, servers.len())?;
        Ok(Self::acquire_compiled(
            name,
            servers,
            startup,
            queue_timeout,
            passive_policy,
            resilience,
            &blueprint,
        ))
    }

    /// Rotation-ordered `(cursor, index, active)` candidates and whether any server is selectable.
    fn candidates(
        &self,
        start: usize,
        excluded: &[String],
        now_unix_ms: u64,
    ) -> (Vec<(usize, usize, u64)>, bool) {
        self.read_health(|endpoints| {
            let pool_available = endpoints
                .iter()
                .any(|server| server.selectable_at(now_unix_ms));
            let eligible =
                |cursor: usize, index: usize| {
                    let server = &endpoints[index];
                    (server.selectable_at(now_unix_ms) && !excluded.contains(&server.name))
                        .then_some((cursor, index, server.active_work.load(Ordering::Acquire)))
                };
            let candidates = if self.weighted_schedule.is_empty() {
                (0..endpoints.len())
                    .filter_map(|offset| {
                        let index = (start + offset) % endpoints.len();
                        eligible(index, index)
                    })
                    .collect()
            } else {
                (0..self.weighted_schedule.len())
                    .filter_map(|offset| {
                        let cursor = (start + offset) % self.weighted_schedule.len();
                        eligible(cursor, self.weighted_schedule[cursor])
                    })
                    .collect()
            };
            (candidates, pool_available)
        })
    }

    /// Servers a pool with nothing selectable may still serve, least bad first.
    fn fail_open_ranking(&self, excluded: &[String]) -> Vec<Arc<PoolEndpoint>> {
        let mut ranked = self.read_health(|endpoints| {
            endpoints
                .iter()
                .filter(|server| server.fail_open_eligible() && !excluded.contains(&server.name))
                .map(|server| (server.fail_open_rank(), Arc::clone(server)))
                .collect::<Vec<_>>()
        });
        ranked.sort_by_key(|(rank, _)| *rank);
        ranked.into_iter().map(|(_, server)| server).collect()
    }

    fn select_fail_open(&self, excluded: &[String]) -> SelectionAttempt {
        let ranking = self.fail_open_ranking(excluded);
        let lease = ranking.iter().find_map(|server| {
            server.try_acquire(
                &self.queue,
                &self.health,
                Admission::FailOpen,
                now_unix_ms(),
            )
        });
        if lease.is_some() {
            self.fail_open_selections.fetch_add(1, Ordering::Relaxed);
        }
        SelectionAttempt {
            lease,
            pool_available: !ranking.is_empty(),
            saturated: ranking.iter().all(|server| !server.has_capacity()),
        }
    }

    pub(super) fn select_server_excluding(&self, excluded: &[String]) -> SelectionAttempt {
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now_unix_ms = now_unix_ms();
        let (candidates, pool_available) = self.candidates(selection.next, excluded, now_unix_ms);
        if !pool_available && self.resilience.fail_open {
            return self.select_fail_open(excluded);
        }
        let saturated = candidates
            .iter()
            .any(|(_, index, _)| !self.endpoints[*index].has_capacity());
        let mut ordered = candidates
            .iter()
            .filter(|(_, index, _)| self.endpoints[*index].has_capacity())
            .copied()
            .collect::<Vec<_>>();
        match &self.algorithm {
            UpstreamAlgorithm::LeastConnections => {
                ordered.sort_by_key(|(_, _, active)| *active);
            }
            UpstreamAlgorithm::First => ordered.sort_by_key(|(_, index, _)| *index),
            UpstreamAlgorithm::WeightedRoundRobin { .. } | UpstreamAlgorithm::RoundRobin => {}
        }
        let selected = ordered.into_iter().find_map(|(cursor, index, _)| {
            self.endpoints[index]
                .try_acquire(
                    &self.queue,
                    &self.health,
                    Admission::Selectable,
                    now_unix_ms,
                )
                .map(|lease| (lease, cursor, index))
        });
        let lease = selected.map(|(lease, cursor, index)| {
            if !matches!(&self.algorithm, UpstreamAlgorithm::First) {
                selection.next = if self.weighted_schedule.is_empty() {
                    (index + 1) % self.endpoints.len()
                } else {
                    (cursor + 1) % self.weighted_schedule.len()
                };
            }
            lease
        });
        SelectionAttempt {
            lease,
            pool_available,
            saturated: saturated && !candidates.is_empty(),
        }
    }

    #[cfg(test)]
    pub(crate) fn select_connection_target_excluding(
        &self,
        excluded: &[String],
    ) -> Option<EndpointLease> {
        let (lease, _) = self.connection_target(excluded);
        if lease.is_none() {
            self.note_unavailable_selection();
        }
        lease
    }

    /// Like [`Self::select_connection_target_excluding`], but waits up to the pool's
    /// `queue_while_unavailable_ms` for a server to become selectable.
    pub(crate) async fn select_connection_target_waiting(
        &self,
        excluded: &[String],
    ) -> Option<EndpointLease> {
        let recovery_deadline = self.recovery_deadline();
        loop {
            let (lease, pool_available) = self.connection_target(excluded);
            if lease.is_some() || pool_available || !self.await_recovery(recovery_deadline).await {
                if lease.is_none() {
                    self.note_unavailable_selection();
                }
                return lease;
            }
        }
    }

    /// A connection target, and whether any server was selectable.
    fn connection_target(&self, excluded: &[String]) -> (Option<EndpointLease>, bool) {
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (candidates, pool_available) = self.candidates(selection.next, excluded, now_unix_ms());
        if !pool_available && self.resilience.fail_open {
            return (self.fail_open_connection_target(excluded), false);
        }
        let available = candidates
            .iter()
            .copied()
            .filter(|(_, index, _)| self.endpoints[*index].has_capacity())
            .collect::<Vec<_>>();
        let candidates = if available.is_empty() {
            &candidates
        } else {
            &available
        };
        let selected = match &self.algorithm {
            UpstreamAlgorithm::WeightedRoundRobin { .. } | UpstreamAlgorithm::RoundRobin => {
                candidates
                    .first()
                    .map(|(cursor, index, _)| (*cursor, *index))
            }
            UpstreamAlgorithm::LeastConnections => candidates
                .iter()
                .min_by_key(|(_, _, active)| *active)
                .map(|(cursor, index, _)| (*cursor, *index)),
            UpstreamAlgorithm::First => candidates
                .iter()
                .min_by_key(|(_, index, _)| *index)
                .map(|(cursor, index, _)| (*cursor, *index)),
        };
        let Some((cursor, index)) = selected else {
            return (None, pool_available);
        };
        if !matches!(&self.algorithm, UpstreamAlgorithm::First) {
            selection.next = if self.weighted_schedule.is_empty() {
                (index + 1) % self.endpoints.len()
            } else {
                (cursor + 1) % self.weighted_schedule.len()
            };
        }
        (
            Some(self.pending_lease(index, Admission::Selectable)),
            pool_available,
        )
    }

    fn fail_open_connection_target(&self, excluded: &[String]) -> Option<EndpointLease> {
        let ranking = self.fail_open_ranking(excluded);
        let server = ranking
            .iter()
            .find(|server| server.has_capacity())
            .or_else(|| ranking.first())?;
        self.fail_open_selections.fetch_add(1, Ordering::Relaxed);
        Some(self.pending_lease_for(server, Admission::FailOpen))
    }

    fn pending_lease(&self, index: usize, admission: Admission) -> EndpointLease {
        self.pending_lease_for(&self.endpoints[index], admission)
    }

    fn pending_lease_for(&self, server: &Arc<PoolEndpoint>, admission: Admission) -> EndpointLease {
        EndpointLease::pending(
            Arc::clone(server),
            Arc::clone(&self.queue),
            self.queue_timeout,
            Arc::clone(&self.health),
            admission,
        )
    }

    #[must_use]
    pub fn select(&self) -> Option<EndpointLease> {
        self.select_excluding(&[])
    }

    #[must_use]
    pub fn select_excluding(&self, excluded: &[RuntimeEndpoint]) -> Option<EndpointLease> {
        let names = self
            .endpoints
            .iter()
            .filter(|server| excluded.contains(&server.endpoint))
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let attempt = self.select_server_excluding(&names);
        if attempt.lease.is_none() && !attempt.pool_available {
            self.note_unavailable_selection();
        }
        attempt.lease
    }

    pub async fn select_wait(&self) -> Option<EndpointLease> {
        self.select_wait_excluding(&[]).await
    }

    pub(crate) async fn select_wait_excluding(&self, excluded: &[String]) -> Option<EndpointLease> {
        self.select_wait_with(|| self.select_server_excluding(excluded))
            .await
    }

    pub(crate) async fn select_server_wait(&self, name: &str) -> Option<EndpointLease> {
        self.select_wait_with(|| self.select_named_server(name))
            .await
    }

    async fn select_wait_with(
        &self,
        mut select: impl FnMut() -> SelectionAttempt,
    ) -> Option<EndpointLease> {
        let recovery_deadline = self.recovery_deadline();
        loop {
            let (lease, unavailable) = self.select_wait_once(&mut select).await;
            if !unavailable || !self.await_recovery(recovery_deadline).await {
                if unavailable {
                    self.note_unavailable_selection();
                }
                return lease;
            }
        }
    }

    /// One queued selection; the flag reports that no server was selectable.
    async fn select_wait_once(
        &self,
        select: &mut impl FnMut() -> SelectionAttempt,
    ) -> (Option<EndpointLease>, bool) {
        let Some(queue_timeout) = self.queue_timeout else {
            let attempt = select();
            let unavailable = attempt.lease.is_none() && !attempt.pool_available;
            return (attempt.lease, unavailable);
        };
        let deadline = Instant::now() + queue_timeout;
        let mut waiting = QueueWaitGuard::new(Arc::clone(&self.queue));
        loop {
            let waiter = Arc::clone(&waiting.waiter);
            let notified = waiter.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if waiting.is_front() {
                let attempt = select();
                if let Some(lease) = attempt.lease {
                    waiting.complete();
                    return (Some(lease), false);
                }
                if !attempt.pool_available || !attempt.saturated {
                    waiting.complete();
                    return (None, !attempt.pool_available);
                }
            }
            waiting.mark_waiting();
            if timeout_at(deadline, notified).await.is_err() {
                waiting.timeout();
                return (None, false);
            }
        }
    }

    fn recovery_deadline(&self) -> Option<Instant> {
        self.resilience
            .unavailable_wait
            .map(|wait| Instant::now() + wait)
    }

    /// Waits for a health change or the next poll, since ejections also end with time. Returns
    /// false when the deadline has passed or no server could ever become selectable by itself.
    async fn await_recovery(&self, deadline: Option<Instant>) -> bool {
        let Some(deadline) = deadline else {
            return false;
        };
        let now = Instant::now();
        let recoverable = self
            .read_health(|endpoints| endpoints.iter().any(|server| server.fail_open_eligible()));
        if now >= deadline || !recoverable {
            return false;
        }
        let _ = timeout_at(
            (now + RECOVERY_POLL).min(deadline),
            self.queue.notify.notified(),
        )
        .await;
        true
    }

    fn select_named_server(&self, name: &str) -> SelectionAttempt {
        let Some(server) = self.endpoints.iter().find(|server| server.name == name) else {
            return SelectionAttempt {
                lease: None,
                pool_available: false,
                saturated: false,
            };
        };
        let now_unix_ms = now_unix_ms();
        let admission = server.admission(now_unix_ms, self.failing_open(now_unix_ms));
        let lease = admission.and_then(|admission| {
            server.try_acquire(&self.queue, &self.health, admission, now_unix_ms)
        });
        if lease.is_some() && admission == Some(Admission::FailOpen) {
            self.fail_open_selections.fetch_add(1, Ordering::Relaxed);
        }
        SelectionAttempt {
            lease,
            pool_available: admission.is_some(),
            saturated: admission.is_some() && !server.has_capacity(),
        }
    }

    pub(crate) fn select_server_connection_target(&self, name: &str) -> Option<EndpointLease> {
        let server = self.endpoints.iter().find(|server| server.name == name)?;
        let now_unix_ms = now_unix_ms();
        let admission = server.admission(now_unix_ms, self.failing_open(now_unix_ms))?;
        if admission == Admission::FailOpen {
            self.fail_open_selections.fetch_add(1, Ordering::Relaxed);
        }
        Some(self.pending_lease_for(server, admission))
    }

    /// Whether the pool serves its least-bad servers because none is selectable.
    fn failing_open(&self, now_unix_ms: u64) -> bool {
        self.resilience.fail_open
            && self.read_health(|endpoints| {
                !endpoints
                    .iter()
                    .any(|server| server.selectable_at(now_unix_ms))
            })
    }

    #[must_use]
    pub fn algorithm(&self) -> UpstreamAlgorithm {
        self.algorithm.clone()
    }

    #[must_use]
    pub fn has_unattempted(&self, attempted: &[RuntimeEndpoint]) -> bool {
        self.has_unattempted_by(|server| attempted.contains(&server.endpoint))
    }

    #[must_use]
    pub(crate) fn has_unattempted_servers(&self, attempted: &[String]) -> bool {
        self.has_unattempted_by(|server| attempted.contains(&server.name))
    }

    fn has_unattempted_by(&self, attempted: impl Fn(&PoolEndpoint) -> bool) -> bool {
        let now_unix_ms = now_unix_ms();
        let failing_open = self.failing_open(now_unix_ms);
        self.read_health(|endpoints| {
            endpoints.iter().any(|server| {
                !attempted(server) && server.admission(now_unix_ms, failing_open).is_some()
            })
        })
    }

    #[must_use]
    pub fn has_available(&self) -> bool {
        let now_unix_ms = now_unix_ms();
        self.read_health(|endpoints| {
            endpoints
                .iter()
                .any(|server| server.selectable_at(now_unix_ms) && server.has_capacity())
        })
    }

    pub(crate) fn note_unavailable_selection(&self) {
        self.unavailable_selections.fetch_add(1, Ordering::Relaxed);
    }

    /// Changes one named server's administrative state. Existing leases continue draining.
    ///
    /// # Errors
    ///
    /// Returns an error when this pool has no server with the exact canonical name.
    pub fn set_server_administrative_state(
        &self,
        name: &str,
        state: AdministrativeState,
    ) -> Result<(), PoolAdminError> {
        let server = self.server(name)?;
        server
            .administrative_state
            .store(state as u8, Ordering::Release);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    pub fn set_administrative_state(&self, state: AdministrativeState) {
        for server in &*self.endpoints {
            server
                .administrative_state
                .store(state as u8, Ordering::Release);
        }
        self.queue.notify_capacity_waiters();
    }

    /// Sets a selection-only health override without changing observed health.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact server name is unknown.
    pub fn set_server_health_override(
        &self,
        name: &str,
        health_override: HealthOverride,
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .health_override
            .store(health_override as u8, Ordering::Release);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    /// Enables or disables configured health probes for one server.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact server name is unknown.
    pub fn set_server_checks_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .checks_enabled
            .store(enabled, Ordering::Release);
        Ok(())
    }

    /// Sets a runtime capacity override, or restores configured capacity with `None`.
    ///
    /// # Errors
    ///
    /// Returns an error for zero capacity or an unknown exact server name.
    pub fn set_server_max_connections(
        &self,
        name: &str,
        limit: Option<u64>,
    ) -> Result<(), PoolAdminError> {
        if limit == Some(0) {
            return Err(PoolAdminError::InvalidMaxConnections);
        }
        self.server(name)?
            .max_connections_override
            .store(limit.unwrap_or(0), Ordering::Release);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    /// Administrative state that differs from a freshly started server's.
    #[must_use]
    pub fn administrative_states(&self) -> Vec<ServerAdminState> {
        self.endpoints
            .iter()
            .map(|server| ServerAdminState {
                pool: self.name.clone(),
                server: server.name.clone(),
                administrative: server.administrative_state(),
                health: server.health_override(),
                checks_enabled: server.checks_enabled.load(Ordering::Acquire),
                max_connections: match server.max_connections_override.load(Ordering::Acquire) {
                    0 => None,
                    limit => Some(limit),
                },
            })
            .filter(|state| !state.is_default())
            .collect()
    }

    /// Applies carried state to the servers that still exist in this pool.
    pub fn restore_administrative_states(&self, states: &[ServerAdminState]) {
        for state in states.iter().filter(|state| state.pool == self.name) {
            let Ok(server) = self.server(&state.server) else {
                continue;
            };
            server
                .administrative_state
                .store(state.administrative as u8, Ordering::Release);
            server
                .health_override
                .store(state.health as u8, Ordering::Release);
            server
                .checks_enabled
                .store(state.checks_enabled, Ordering::Release);
            server
                .max_connections_override
                .store(state.max_connections.unwrap_or(0), Ordering::Release);
        }
        self.queue.notify_capacity_waiters();
    }

    #[must_use]
    pub fn has_server(&self, name: &str) -> bool {
        self.endpoints.iter().any(|server| server.name == name)
    }

    fn server(&self, name: &str) -> Result<&Arc<PoolEndpoint>, PoolAdminError> {
        self.endpoints
            .iter()
            .find(|server| server.name == name)
            .ok_or_else(|| PoolAdminError::UnknownServer(name.to_owned()))
    }

    pub(crate) fn endpoints(&self) -> impl Iterator<Item = (usize, RuntimeEndpoint)> + '_ {
        self.endpoints
            .iter()
            .enumerate()
            .map(|(index, server)| (index, server.endpoint.clone()))
    }

    pub(crate) fn record_health(
        &self,
        index: usize,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
    ) -> Option<(EndpointHealthState, EndpointHealthState)> {
        let update = self.endpoints.get(index).map(|server| {
            self.health.record_health(
                server,
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
            )
        });
        update.and_then(|update| update.transition)
    }

    #[cfg(test)]
    pub(crate) fn record_passive_failure(&self, index: usize, failure: HealthFailure) {
        if let Some(server) = self.endpoints.get(index) {
            self.health.record_passive_failure(server, failure);
        }
    }

    #[cfg(test)]
    pub(super) fn record_passive_failure_at(
        &self,
        index: usize,
        failure: HealthFailure,
        at_unix_ms: u64,
    ) {
        if let Some(server) = self.endpoints.get(index) {
            self.health
                .record_passive_failure_at(server, failure, at_unix_ms);
        }
    }

    pub(crate) fn health_state(&self, index: usize) -> Option<EndpointHealthState> {
        self.endpoints.get(index).map(|server| server.state())
    }

    pub(crate) fn health_checks_running(&self, index: usize) -> bool {
        self.endpoints
            .get(index)
            .is_some_and(|server| server.checks_running())
    }

    pub(crate) fn passive_ejected(&self, index: usize) -> bool {
        self.endpoints
            .get(index)
            .is_some_and(|server| server.passively_ejected_at(now_unix_ms()))
    }

    /// Resolves one server immediately without mutating runtime state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown server or failed/empty DNS resolution.
    pub async fn resolve_server_dns(&self, name: &str) -> Result<Vec<SocketAddr>, PoolAdminError> {
        let server = Arc::clone(self.server(name)?);
        server
            .resolve_fresh_dns()
            .await
            .map_err(|_| PoolAdminError::DnsRefreshFailed)
    }

    /// Commits an already-resolved address set without performing external I/O.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown server, a non-DNS endpoint, or a protected address.
    pub fn commit_server_dns(
        &self,
        name: &str,
        addresses: &[SocketAddr],
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .commit_dns(addresses)
            .map_err(|_| PoolAdminError::DnsRefreshFailed)
    }

    pub(crate) async fn resolve_server_addresses(
        &self,
        index: usize,
    ) -> io::Result<Vec<SocketAddr>> {
        let server = self.endpoints.get(index).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "upstream server index is invalid")
        })?;
        server.resolve_addresses().await
    }

    #[must_use]
    pub fn health_snapshot(&self) -> PoolHealthSnapshot {
        let now_unix_ms = now_unix_ms();
        let endpoints = self.read_health(|endpoints| {
            endpoints
                .iter()
                .map(|server| server.snapshot(now_unix_ms))
                .collect::<Vec<_>>()
        });
        PoolHealthSnapshot {
            name: self.name.clone(),
            algorithm: algorithm_name(&self.algorithm),
            available_endpoints: endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint.administrative_state == AdministrativeState::Ready
                        && !endpoint.passive_ejected
                        && match endpoint.health_override {
                            HealthOverride::Auto => endpoint.state.selectable(),
                            HealthOverride::Up => true,
                            HealthOverride::Down => false,
                        }
                })
                .count(),
            total_endpoints: endpoints.len(),
            unavailable_selections: self.unavailable_selections.load(Ordering::Relaxed),
            fail_open_selections: self.fail_open_selections.load(Ordering::Relaxed),
            queued: self.queue.queued.load(Ordering::Relaxed),
            queued_total: self.queue.queued_total.load(Ordering::Relaxed),
            queue_timeouts: self.queue.timeouts.load(Ordering::Relaxed),
            queue_cancellations: self.queue.cancellations.load(Ordering::Relaxed),
            endpoints,
        }
    }

    fn read_health<T>(&self, read: impl Fn(&[Arc<PoolEndpoint>]) -> T) -> T {
        for _ in 0..8 {
            let version = self.health.health_version.load(Ordering::Acquire);
            if !version.is_multiple_of(2) {
                std::thread::yield_now();
                continue;
            }
            let value = read(&self.endpoints);
            fence(Ordering::Acquire);
            if self.health.health_version.load(Ordering::Relaxed) == version {
                return value;
            }
        }
        let _writer = self
            .health
            .health_writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read(&self.endpoints)
    }
}
