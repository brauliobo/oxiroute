#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn nonempty<T>(items: impl IntoIterator<Item = T>) -> Result<Vec<T>, PoolError> {
    let items = items.into_iter().collect::<Vec<_>>();
    if items.is_empty() {
        Err(PoolError::Empty)
    } else {
        Ok(items)
    }
}

const MAX_SERVER_WEIGHT: u16 = 100;

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
            .map(|(server, weight)| {
                Arc::new(PoolEndpoint::new(
                    server,
                    startup,
                    weight,
                    ServerOrigin::Static,
                    0,
                    passive_policy.circuit_breaker,
                ))
            })
            .collect::<Vec<_>>();
        let queue = Arc::new(PoolQueue::new(
            queue_timeout.is_some(),
            blueprint.queue.clone(),
        ));
        let health = Arc::new(PoolHealthState::new(
            name.clone(),
            Membership::new(endpoints),
            Arc::clone(&queue),
            passive_policy,
            resilience,
            blueprint.readmission.clone(),
        ));
        Self {
            algorithm: blueprint.algorithm.clone(),
            name,
            startup,
            health,
            selection: Mutex::new(SelectionState::new(blueprint.weights.len())),
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
        Self::new_named(String::new(), nonempty(endpoints)?, algorithm, false)
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
            nonempty(endpoints)?
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
        let blueprint = PoolConstructionBlueprint::compile(&algorithm, servers.len(), None, None)?;
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

    /// The current membership. Selection holds the selection lock, which membership changes also
    /// take, so the snapshot it reads stays the pool's membership for the whole selection.
    fn members(&self) -> Arc<Membership> {
        self.health.members.load_full()
    }

    /// Rotation-ordered candidates and whether any server is selectable.
    fn candidates(
        &self,
        members: &Membership,
        start: usize,
        excluded: &[String],
        now_unix_ms: u64,
    ) -> (Vec<Candidate>, bool) {
        self.read_health_of(members, |members| {
            let endpoints = &members.endpoints;
            let pool_available = endpoints
                .iter()
                .any(|server| server.selectable_at(now_unix_ms));
            let candidates = (0..endpoints.len())
                .filter_map(|offset| {
                    let index = (start + offset) % endpoints.len();
                    let server = &endpoints[index];
                    (server.selectable_at(now_unix_ms) && !excluded.contains(&server.name)).then(
                        || Candidate {
                            index,
                            load: server.load(),
                            weight: server.weight(),
                        },
                    )
                })
                .collect();
            (candidates, pool_available)
        })
    }

    /// Orders `candidates` by the algorithm's preference; the first is its pick.
    fn rank(&self, selection: &mut SelectionState, candidates: &mut [Candidate]) {
        match &self.algorithm {
            UpstreamAlgorithm::RoundRobin => {}
            UpstreamAlgorithm::First => candidates.sort_by_key(|candidate| candidate.index),
            UpstreamAlgorithm::LeastConnections => {
                candidates.sort_by_key(|candidate| candidate.load);
            }
            UpstreamAlgorithm::WeightedRoundRobin { .. } => {
                candidates.sort_by_key(|candidate| {
                    (
                        std::cmp::Reverse(
                            selection.credit[candidate.index] + i64::from(candidate.weight),
                        ),
                        candidate.index,
                    )
                });
            }
            UpstreamAlgorithm::PowerOfTwoChoices => {
                if candidates.len() > 1 {
                    let first =
                        usize::try_from(selection.next_random()).unwrap_or(0) % candidates.len();
                    let mut second = usize::try_from(selection.next_random()).unwrap_or(0)
                        % (candidates.len() - 1);
                    if second >= first {
                        second += 1;
                    }
                    let (better, other) = if candidates[second].load < candidates[first].load {
                        (second, first)
                    } else {
                        (first, second)
                    };
                    candidates.swap(0, better);
                    let other = if other == 0 { better } else { other };
                    candidates.swap(1, other);
                }
            }
        }
    }

    /// Records that `chosen` was picked among `candidates`.
    fn commit(
        &self,
        selection: &mut SelectionState,
        members: &Membership,
        candidates: &[Candidate],
        chosen: usize,
    ) {
        if matches!(
            &self.algorithm,
            UpstreamAlgorithm::WeightedRoundRobin { .. }
        ) {
            let total = candidates
                .iter()
                .map(|candidate| i64::from(candidate.weight))
                .sum::<i64>();
            for candidate in candidates {
                selection.credit[candidate.index] += i64::from(candidate.weight);
            }
            selection.credit[chosen] -= total;
        }
        if !matches!(&self.algorithm, UpstreamAlgorithm::First) {
            selection.next = (chosen + 1) % members.endpoints.len();
        }
    }

    /// Where candidates start: weighted rotation must not depend on the last pick.
    fn rotation_start(&self, selection: &SelectionState) -> usize {
        match &self.algorithm {
            UpstreamAlgorithm::WeightedRoundRobin { .. } => 0,
            _ => selection.next,
        }
    }

    /// Servers a pool with nothing selectable may still serve, least bad first.
    fn fail_open_ranking(&self, excluded: &[String]) -> Vec<Arc<PoolEndpoint>> {
        let mut ranked = self.read_health(|members| {
            members
                .endpoints
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
        self.select_attempt(excluded, false)
    }

    /// Acquires a permit of the best server. With `reuse_idle`, a server whose pooled connection
    /// is idle also qualifies and gets a pending lease, since the connection serves the request
    /// without a new permit.
    fn select_attempt(&self, excluded: &[String], reuse_idle: bool) -> SelectionAttempt {
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let members = self.members();
        let now_unix_ms = now_unix_ms();
        let start = self.rotation_start(&selection);
        let (candidates, pool_available) = self.candidates(&members, start, excluded, now_unix_ms);
        if !pool_available && self.resilience.fail_open {
            return if reuse_idle {
                SelectionAttempt {
                    lease: self.fail_open_connection_target(excluded),
                    pool_available: false,
                    saturated: false,
                }
            } else {
                self.select_fail_open(excluded)
            };
        }
        let usable = |server: &PoolEndpoint| {
            server.has_capacity() || (reuse_idle && server.has_idle_connection())
        };
        let saturated = candidates
            .iter()
            .any(|candidate| !usable(&members.endpoints[candidate.index]));
        let has_candidates = !candidates.is_empty();
        let mut ordered = candidates
            .into_iter()
            .filter(|candidate| usable(&members.endpoints[candidate.index]))
            .collect::<Vec<_>>();
        let pool = ordered.clone();
        self.rank(&mut selection, &mut ordered);
        let selected = ordered.iter().find_map(|candidate| {
            let server = &members.endpoints[candidate.index];
            server
                .try_acquire(
                    &self.queue,
                    &self.health,
                    Admission::Selectable,
                    now_unix_ms,
                )
                .or_else(|| {
                    (reuse_idle && server.has_idle_connection()).then(|| {
                        self.pending_lease_for(
                            &members.endpoints[candidate.index],
                            Admission::Selectable,
                        )
                    })
                })
                .map(|lease| (lease, candidate.index))
        });
        let lease = selected.map(|(lease, index)| {
            self.commit(&mut selection, &members, &pool, index);
            lease
        });
        SelectionAttempt {
            lease,
            pool_available,
            saturated: saturated && has_candidates,
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
    /// `queue_while_unavailable_ms` for a server to become selectable. A pool with a queue also
    /// holds the request there until any server frees a permit or an idle connection, so it is
    /// never bound to one busy server while another is free.
    pub(crate) async fn select_connection_target_waiting(
        &self,
        excluded: &[String],
        tag: QueueTag,
    ) -> Option<EndpointLease> {
        if self.queue_timeout.is_some() {
            return self
                .select_wait_with(tag, || self.select_attempt(excluded, true))
                .await;
        }
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
        let members = self.members();
        let start = self.rotation_start(&selection);
        let (candidates, pool_available) =
            self.candidates(&members, start, excluded, now_unix_ms());
        if !pool_available && self.resilience.fail_open {
            return (self.fail_open_connection_target(excluded), false);
        }
        let available = candidates
            .iter()
            .copied()
            .filter(|candidate| members.endpoints[candidate.index].has_capacity())
            .collect::<Vec<_>>();
        let mut pool = if available.is_empty() {
            candidates
        } else {
            available
        };
        let pooled = pool.clone();
        self.rank(&mut selection, &mut pool);
        let Some(chosen) = pool.first().map(|candidate| candidate.index) else {
            return (None, pool_available);
        };
        self.commit(&mut selection, &members, &pooled, chosen);
        (
            Some(self.pending_lease_for(&members.endpoints[chosen], Admission::Selectable)),
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
            .members()
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
        self.select_wait_excluding(&[], QueueTag::default()).await
    }

    pub(crate) async fn select_wait_excluding(
        &self,
        excluded: &[String],
        tag: QueueTag,
    ) -> Option<EndpointLease> {
        self.select_wait_with(tag, || self.select_server_excluding(excluded))
            .await
    }

    pub(crate) async fn select_server_wait(
        &self,
        name: &str,
        tag: QueueTag,
    ) -> Option<EndpointLease> {
        self.select_wait_with(tag, || self.select_named_server(name))
            .await
    }

    /// Where a request queues in this pool: the route class, else the class header, plus the tenant.
    pub(crate) fn queue_tag(&self, route_class: Option<&str>, headers: &HeaderMap) -> QueueTag {
        self.queue
            .schedule
            .as_ref()
            .map_or_else(QueueTag::default, |schedule| {
                schedule.tag(route_class, headers)
            })
    }

    async fn select_wait_with(
        &self,
        tag: QueueTag,
        mut select: impl FnMut() -> SelectionAttempt,
    ) -> Option<EndpointLease> {
        let recovery_deadline = self.recovery_deadline();
        loop {
            let (lease, unavailable) = self.select_wait_once(tag, &mut select).await;
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
        tag: QueueTag,
        select: &mut impl FnMut() -> SelectionAttempt,
    ) -> (Option<EndpointLease>, bool) {
        let Some(queue_timeout) = self.queue_timeout else {
            let attempt = select();
            let unavailable = attempt.lease.is_none() && !attempt.pool_available;
            return (attempt.lease, unavailable);
        };
        let deadline = Instant::now() + queue_timeout;
        let mut waiting = QueueWaitGuard::new(Arc::clone(&self.queue), &self.name, tag);
        loop {
            let waiter = Arc::clone(&waiting.waiter);
            let notified = waiter.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if waiting.is_front() {
                let attempt = select();
                if let Some(lease) = attempt.lease {
                    lease.note_selector_position(waiting.position());
                    waiting.serve();
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
        let recoverable = self.read_health(|members| {
            members
                .endpoints
                .iter()
                .any(|server| server.fail_open_eligible())
        });
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

    pub(super) fn select_named_server(&self, name: &str) -> SelectionAttempt {
        let Some(server) = self.server_endpoint(name) else {
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
        let server = self.server_endpoint(name)?;
        let now_unix_ms = now_unix_ms();
        let admission = server.admission(now_unix_ms, self.failing_open(now_unix_ms))?;
        if admission == Admission::FailOpen {
            self.fail_open_selections.fetch_add(1, Ordering::Relaxed);
        }
        Some(self.pending_lease_for(&server, admission))
    }

    /// Whether the pool serves its least-bad servers because none is selectable.
    fn failing_open(&self, now_unix_ms: u64) -> bool {
        self.resilience.fail_open
            && self.read_health(|members| {
                !members
                    .endpoints
                    .iter()
                    .any(|server| server.selectable_at(now_unix_ms))
            })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
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
        self.read_health(|members| {
            members.endpoints.iter().any(|server| {
                !attempted(server) && server.admission(now_unix_ms, failing_open).is_some()
            })
        })
    }

    #[must_use]
    pub fn has_available(&self) -> bool {
        let now_unix_ms = now_unix_ms();
        self.read_health(|members| {
            members
                .endpoints
                .iter()
                .any(|server| server.selectable_at(now_unix_ms) && server.has_capacity())
        })
    }

    /// Name and weight of every server that could be chosen now, in pool order.
    pub(crate) fn selectable_servers(&self) -> Vec<(String, u16)> {
        let now_unix_ms = now_unix_ms();
        self.read_health(|members| {
            members
                .endpoints
                .iter()
                .filter(|server| server.selectable_at(now_unix_ms))
                .map(|server| (server.name.clone(), server.weight()))
                .collect()
        })
    }

    /// Whether any server could be chosen now, ignoring whether it has spare capacity.
    #[must_use]
    pub fn has_selectable(&self) -> bool {
        let now_unix_ms = now_unix_ms();
        self.read_health(|members| {
            members
                .endpoints
                .iter()
                .any(|server| server.selectable_at(now_unix_ms))
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
        for server in &*self.members().endpoints {
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

    /// Sets a runtime weight override, or restores the configured weight with `None`.
    ///
    /// # Errors
    ///
    /// Returns an error for a weight outside 1..=100 or an unknown exact server name.
    pub fn set_server_weight(&self, name: &str, weight: Option<u16>) -> Result<(), PoolAdminError> {
        if weight.is_some_and(|weight| !(1..=MAX_SERVER_WEIGHT).contains(&weight)) {
            return Err(PoolAdminError::InvalidWeight);
        }
        self.server(name)?
            .weight_override
            .store(weight.unwrap_or(0), Ordering::Release);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    /// Administrative state that differs from a freshly started server's.
    #[must_use]
    pub fn administrative_states(&self) -> Vec<ServerAdminState> {
        self.members()
            .endpoints
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
                weight: match server.weight_override.load(Ordering::Acquire) {
                    0 => None,
                    weight => Some(weight),
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
            server
                .weight_override
                .store(state.weight.unwrap_or(0), Ordering::Release);
        }
        self.queue.notify_capacity_waiters();
    }

    #[must_use]
    pub fn has_server(&self, name: &str) -> bool {
        self.server_endpoint(name).is_some()
    }

    fn server_endpoint(&self, name: &str) -> Option<Arc<PoolEndpoint>> {
        let members = self.members();
        members
            .position(name)
            .map(|index| Arc::clone(&members.endpoints[index]))
    }

    fn server(&self, name: &str) -> Result<Arc<PoolEndpoint>, PoolAdminError> {
        self.server_endpoint(name)
            .ok_or_else(|| PoolAdminError::UnknownServer(name.to_owned()))
    }

    /// Origin and endpoint of the member with this name.
    pub(crate) fn member(&self, name: &str) -> Option<(ServerOrigin, RuntimeEndpoint)> {
        self.server_endpoint(name)
            .map(|server| (server.origin, server.endpoint.clone()))
    }

    /// The member that already uses `endpoint`, if any.
    pub(crate) fn member_at(&self, endpoint: &RuntimeEndpoint) -> Option<String> {
        self.members()
            .endpoints
            .iter()
            .find(|server| server.endpoint == *endpoint)
            .map(|server| server.name.clone())
    }

    #[cfg(test)]
    pub(crate) fn server_handle(&self, name: &str) -> Option<ServerHandle> {
        self.server_endpoint(name).map(ServerHandle)
    }

    pub(crate) fn endpoints(&self) -> impl Iterator<Item = (ServerHandle, RuntimeEndpoint)> {
        self.members()
            .endpoints
            .iter()
            .map(|server| (ServerHandle(Arc::clone(server)), server.endpoint.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[cfg(test)]
    pub(crate) fn handle(&self, index: usize) -> ServerHandle {
        ServerHandle(Arc::clone(&self.members().endpoints[index]))
    }

    pub(crate) fn record_health(
        &self,
        server: &ServerHandle,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
    ) -> Option<(EndpointHealthState, EndpointHealthState)> {
        self.health
            .record_health(
                &server.0,
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
            )
            .transition
    }

    #[cfg(test)]
    pub(crate) fn record_passive_failure(&self, index: usize, failure: HealthFailure) {
        if let Some(server) = self.members().endpoints.get(index) {
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
        if let Some(server) = self.members().endpoints.get(index) {
            self.health
                .record_passive_failure_at(server, failure, at_unix_ms);
        }
    }

    /// Resolves one server immediately without mutating runtime state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown server or failed/empty DNS resolution.
    pub async fn resolve_server_dns(&self, name: &str) -> Result<Vec<SocketAddr>, PoolAdminError> {
        let server = self.server(name)?;
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
        server: &ServerHandle,
    ) -> io::Result<Vec<SocketAddr>> {
        server.0.resolve_addresses().await
    }

    #[must_use]
    pub fn health_snapshot(&self) -> PoolHealthSnapshot {
        let now_unix_ms = now_unix_ms();
        let endpoints = self.read_health(|members| {
            members
                .endpoints
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
            queued_by_class: self.queue.depth_by_class(),
            oldest_queue_wait_ms: u64::try_from(self.queue.ledger.oldest().as_millis())
                .unwrap_or(u64::MAX),
            queued_total: self.queue.queued_total.load(Ordering::Relaxed),
            queue_timeouts: self.queue.timeouts.load(Ordering::Relaxed),
            queue_cancellations: self.queue.cancellations.load(Ordering::Relaxed),
            canary: self.health.canary.get().map(|state| state.snapshot()),
            endpoints,
        }
    }

    fn read_health<T>(&self, read: impl Fn(&Membership) -> T) -> T {
        self.read_health_of(&self.members(), read)
    }

    fn read_health_of<T>(&self, members: &Membership, read: impl Fn(&Membership) -> T) -> T {
        for _ in 0..8 {
            let version = self.health.health_version.load(Ordering::Acquire);
            if !version.is_multiple_of(2) {
                std::thread::yield_now();
                continue;
            }
            let value = read(members);
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
        read(members)
    }

    /// Adds a server that registered at runtime. Static servers stay first in file order.
    ///
    /// # Errors
    ///
    /// Returns an error for a name already in the pool or an endpoint the runtime cannot represent.
    pub(crate) fn add_server(
        &self,
        server: RuntimeServer,
        spec: DynamicServer,
    ) -> Result<ServerHandle, PoolMembershipError> {
        server.endpoint.preflight()?;
        let mut selection = self.lock_selection();
        let current = self.members();
        if current.position(&server.name).is_some() {
            return Err(PoolMembershipError::DuplicateName(server.name));
        }
        let endpoint = Arc::new(PoolEndpoint::new(
            server,
            self.startup,
            spec.weight,
            ServerOrigin::Dynamic,
            spec.priority,
            self.health.breaker_policy(),
        ));
        let mut endpoints = current.endpoints.to_vec();
        endpoints.insert(
            current.dynamic_position(spec.priority, &endpoint.name),
            Arc::clone(&endpoint),
        );
        self.install(&mut selection, &current, endpoints);
        Ok(ServerHandle(endpoint))
    }

    /// Removes a server from selection. Leases it already handed out keep counting against it.
    ///
    /// # Errors
    ///
    /// Returns an error when the pool has no server with that name.
    pub(crate) fn remove_server(&self, name: &str) -> Result<ServerHandle, PoolMembershipError> {
        let mut selection = self.lock_selection();
        let current = self.members();
        let at = current
            .position(name)
            .ok_or_else(|| PoolMembershipError::UnknownServer(name.to_owned()))?;
        let removed = Arc::clone(&current.endpoints[at]);
        let mut endpoints = current.endpoints.to_vec();
        endpoints.remove(at);
        self.install(&mut selection, &current, endpoints);
        Ok(ServerHandle(removed))
    }

    /// Changes the weight, priority and registered capacity of a member without touching its
    /// health, leases or administrative state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown name.
    pub(crate) fn update_server(
        &self,
        name: &str,
        spec: DynamicServer,
        max_connections: Option<u64>,
    ) -> Result<(), PoolMembershipError> {
        let mut selection = self.lock_selection();
        let current = self.members();
        let server = self
            .server_endpoint(name)
            .ok_or_else(|| PoolMembershipError::UnknownServer(name.to_owned()))?;
        server.set_configured_weight(spec.weight);
        server.priority.store(spec.priority, Ordering::Release);
        server
            .configured_max_connections
            .store(max_connections.unwrap_or(0), Ordering::Release);
        let mut endpoints = current.endpoints.to_vec();
        let statics = endpoints
            .iter()
            .filter(|member| member.origin == ServerOrigin::Static)
            .count();
        endpoints[statics..].sort_by(|a, b| (a.priority(), &a.name).cmp(&(b.priority(), &b.name)));
        self.install(&mut selection, &current, endpoints);
        Ok(())
    }

    fn lock_selection(&self) -> std::sync::MutexGuard<'_, SelectionState> {
        self.selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Publishes a new membership, keeping each surviving server's weighted credit and the
    /// rotation cursor on the same server, and wakes everything that waits on the old one.
    fn install(
        &self,
        selection: &mut SelectionState,
        previous: &Membership,
        endpoints: Vec<Arc<PoolEndpoint>>,
    ) {
        let position = |members: &[Arc<PoolEndpoint>], server: &Arc<PoolEndpoint>| {
            members
                .iter()
                .position(|member| Arc::ptr_eq(member, server))
        };
        selection.credit = endpoints
            .iter()
            .map(|server| {
                position(&previous.endpoints, server).map_or(0, |index| selection.credit[index])
            })
            .collect();
        let next = previous
            .endpoints
            .get(selection.next)
            .and_then(|server| position(&endpoints, server))
            .unwrap_or(selection.next);
        selection.next = next.checked_rem(endpoints.len()).unwrap_or(0);
        self.health
            .members
            .store(Arc::new(Membership::new(endpoints)));
        self.queue.notify_capacity_waiters();
    }
}
