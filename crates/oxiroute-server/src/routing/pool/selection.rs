#[allow(clippy::wildcard_imports)]
use super::*;

impl EndpointPool {
    /// The current membership. Selection holds the selection lock, which membership changes also
    /// take, so the snapshot it reads stays the pool's membership for the whole selection.
    pub(super) fn members(&self) -> Arc<Membership> {
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

    pub(in super::super) fn select_server_excluding(
        &self,
        excluded: &[String],
    ) -> SelectionAttempt {
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
        if self.queue.is_unqueued() {
            let attempt = select();
            if attempt.lease.is_some() {
                return (attempt.lease, false);
            }
            if !attempt.pool_available || !attempt.saturated {
                return (None, !attempt.pool_available);
            }
        }
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

    pub(in super::super) fn select_named_server(&self, name: &str) -> SelectionAttempt {
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
    pub(super) fn failing_open(&self, now_unix_ms: u64) -> bool {
        self.resilience.fail_open
            && self.read_health(|members| {
                !members
                    .endpoints
                    .iter()
                    .any(|server| server.selectable_at(now_unix_ms))
            })
    }
}
