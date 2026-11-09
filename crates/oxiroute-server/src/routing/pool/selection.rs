#[allow(clippy::wildcard_imports)]
use super::*;

/// A fail-open candidate's place in the ranking: its rank, then its position in the pool.
type FailOpenKey = ((bool, u64, u64), usize);

/// The server a selection settled on, and whether the request already holds a capacity permit.
enum Choice {
    Acquired(Arc<PoolEndpoint>),
    /// A pooled idle connection, reserved for the request, serves it; the lease acquires a permit
    /// only if it connects.
    Pending(Arc<PoolEndpoint>),
}

/// What the selection lock protected: the choice, or the verdict that the pool fails open.
enum Chosen {
    FailOpen,
    Selectable {
        choice: Option<Choice>,
        pool_available: bool,
        saturated: bool,
    },
}

impl EndpointPool {
    /// The current membership. Selection holds the selection lock, which membership changes also
    /// take, so the snapshot it reads stays the pool's membership for the whole selection.
    pub(super) fn members(&self) -> Arc<Membership> {
        self.health.members.load_full()
    }

    /// The next server, least bad first, that a pool with nothing selectable may still serve and
    /// that has capacity, after `after`. Reads the ranking in place; a request holds no list of it.
    fn next_fail_open(
        &self,
        excluded: &[String],
        after: Option<FailOpenKey>,
    ) -> Option<(FailOpenKey, Arc<PoolEndpoint>)> {
        self.read_health(|members| {
            members
                .endpoints
                .iter()
                .enumerate()
                .filter(|(_, server)| {
                    server.fail_open_eligible()
                        && server.has_capacity()
                        && !excluded.contains(&server.name)
                })
                .map(|(index, server)| ((server.fail_open_rank(), index), server))
                .filter(|(key, _)| after.is_none_or(|after| *key > after))
                .min_by_key(|(key, _)| *key)
                .map(|(key, server)| (key, Arc::clone(server)))
        })
    }

    /// Whether any server could serve a failing-open pool once it has capacity.
    fn fail_open_candidates_exist(&self, excluded: &[String]) -> bool {
        self.read_health(|members| {
            members
                .endpoints
                .iter()
                .any(|server| server.fail_open_eligible() && !excluded.contains(&server.name))
        })
    }

    fn select_fail_open(&self, excluded: &[String]) -> SelectionAttempt {
        let now_unix_ms = now_unix_ms();
        let mut after = None;
        while let Some((key, server)) = self.next_fail_open(excluded, after) {
            if let Some(lease) =
                server.try_acquire(&self.queue, &self.health, Admission::FailOpen, now_unix_ms)
            {
                self.fail_open_selections.fetch_add(1, Ordering::Relaxed);
                return SelectionAttempt {
                    lease: Some(lease),
                    pool_available: true,
                    saturated: false,
                };
            }
            after = Some(key);
        }
        SelectionAttempt {
            lease: None,
            pool_available: self.fail_open_candidates_exist(excluded),
            saturated: after.is_none(),
        }
    }

    pub(in super::super) fn select_server_excluding(
        &self,
        excluded: &[String],
    ) -> SelectionAttempt {
        self.select_attempt(excluded, false)
    }

    /// The algorithm's best eligible server, falling back to the next best when a permit is lost to a
    /// concurrent selection. The lease is built by the caller once the selection lock is released.
    fn choose_best(
        &self,
        selection: &mut SelectionState,
        members: &Membership,
        view: &SelectableView,
        eligible: Eligibility,
        now_unix_ms: u64,
    ) -> Option<Choice> {
        let reuse_idle = matches!(eligible.capacity, Capacity::PermitOrIdle);
        let mut tried = Vec::new();
        loop {
            let index = self.pick(selection, members, view, &eligible.without(&tried))?;
            let server = &members.endpoints[index];
            let choice =
                if server.try_acquire_permit(&self.queue, Admission::Selectable, now_unix_ms) {
                    Some(Choice::Acquired(Arc::clone(server)))
                } else if reuse_idle && server.reserve_idle(&self.queue) {
                    Some(Choice::Pending(Arc::clone(server)))
                } else {
                    None
                };
            if choice.is_some() {
                self.commit(selection, members, view, &eligible, index);
                return choice;
            }
            tried.push(index);
        }
    }

    fn lease_of(&self, choice: Choice) -> EndpointLease {
        match choice {
            Choice::Acquired(server) => {
                EndpointLease::acquired(server, Arc::clone(&self.queue), Arc::clone(&self.health))
            }
            Choice::Pending(server) => self
                .pending_lease_for(&server, Admission::Selectable)
                .reserved(),
        }
    }

    /// Acquires a permit of the best server. With `reuse_idle`, a server whose pooled connection
    /// is idle also qualifies and gets a pending lease, since the connection serves the request
    /// without a new permit.
    fn select_attempt(&self, excluded: &[String], reuse_idle: bool) -> SelectionAttempt {
        let now_unix_ms = now_unix_ms();
        match self.choose(excluded, reuse_idle, now_unix_ms) {
            Chosen::FailOpen if reuse_idle => SelectionAttempt {
                lease: self.fail_open_connection_target(excluded),
                pool_available: false,
                saturated: false,
            },
            Chosen::FailOpen => self.select_fail_open(excluded),
            Chosen::Selectable {
                choice,
                pool_available,
                saturated,
            } => SelectionAttempt {
                lease: choice.map(|choice| self.lease_of(choice)),
                pool_available,
                saturated,
            },
        }
    }

    /// The selection proper, under the selection lock; leases are built after it is released.
    fn choose(&self, excluded: &[String], reuse_idle: bool, now_unix_ms: u64) -> Chosen {
        let mut selection = self.lock_selection();
        let members = self.health.members.load();
        let view = self.health.selectable_view(&members, now_unix_ms);
        let pool_available = !view.is_empty();
        if !pool_available && self.resilience.fail_open {
            return Chosen::FailOpen;
        }
        let eligible = Eligibility::new(excluded, Capacity::reusing_idle(reuse_idle));
        let choice = pool_available
            .then(|| self.choose_best(&mut selection, &members, &view, eligible, now_unix_ms))
            .flatten();
        let saturated = choice.is_none()
            && view
                .rotation(0)
                .any(|index| eligible.saturates(&members, index));
        Chosen::Selectable {
            choice,
            pool_available,
            saturated,
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
        let now_unix_ms = now_unix_ms();
        let chosen = {
            let mut selection = self.lock_selection();
            let members = self.health.members.load();
            let view = self.health.selectable_view(&members, now_unix_ms);
            if view.is_empty() {
                None
            } else {
                let with_capacity = Eligibility::new(excluded, Capacity::Permit);
                let any = Eligibility::new(excluded, Capacity::Any);
                Some(
                    self.take(&mut selection, &members, &view, &with_capacity)
                        .or_else(|| self.take(&mut selection, &members, &view, &any))
                        .map(|index| Arc::clone(&members.endpoints[index])),
                )
            }
        };
        match chosen {
            None => {
                let lease = self
                    .resilience
                    .fail_open
                    .then(|| self.fail_open_connection_target(excluded))
                    .flatten();
                (lease, false)
            }
            Some(server) => (
                server.map(|server| self.pending_lease_for(&server, Admission::Selectable)),
                true,
            ),
        }
    }

    fn fail_open_connection_target(&self, excluded: &[String]) -> Option<EndpointLease> {
        let server = match self.next_fail_open(excluded, None) {
            Some((_, server)) => server,
            None => self.read_health(|members| {
                members
                    .endpoints
                    .iter()
                    .filter(|server| {
                        server.fail_open_eligible() && !excluded.contains(&server.name)
                    })
                    .min_by_key(|server| server.fail_open_rank())
                    .map(Arc::clone)
            })?,
        };
        self.fail_open_selections.fetch_add(1, Ordering::Relaxed);
        Some(self.pending_lease_for(&server, Admission::FailOpen))
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
        let names = if excluded.is_empty() {
            Vec::new()
        } else {
            self.members()
                .endpoints
                .iter()
                .filter(|server| excluded.contains(&server.endpoint))
                .map(|server| server.name.clone())
                .collect()
        };
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
        let _watch = self.queue.watch();
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
            && self
                .health
                .selectable_view(&self.members(), now_unix_ms)
                .is_empty()
    }
}
