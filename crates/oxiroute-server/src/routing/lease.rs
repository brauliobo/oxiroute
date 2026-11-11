#[allow(clippy::wildcard_imports)]
use super::*;
use crate::upstream_peer::ReusePeer;

/// A health-aware selector over a named-server list that registered servers may join and leave.
#[derive(Debug)]
pub struct EndpointPool {
    pub(super) algorithm: UpstreamAlgorithm,
    pub(super) name: String,
    pub(super) startup: Option<HealthStartup>,
    pub(super) health: Arc<PoolHealthState>,
    pub(super) selection: Mutex<SelectionState>,
    pub(super) queue: Arc<PoolQueue>,
    pub(super) queue_timeout: Option<std::time::Duration>,
    pub(super) resilience: PoolResiliencePolicy,
    pub(super) unavailable_selections: AtomicU64,
    pub(super) fail_open_selections: AtomicU64,
}

#[derive(Debug)]
pub(super) struct SelectionState {
    pub(super) next: usize,
    /// Smooth weighted round robin: each server's accumulated credit, unless `weights` holds it.
    pub(super) credit: Vec<i64>,
    pub(super) weights: WeightIndex,
    /// Least connections: the loads of the selectable servers.
    pub(super) loads: LoadIndex,
    random: u64,
}

impl SelectionState {
    pub(super) fn new(servers: usize) -> Self {
        Self {
            next: 0,
            credit: vec![0; servers],
            weights: WeightIndex::default(),
            loads: LoadIndex::default(),
            random: ejection_entropy(0) | 1,
        }
    }

    /// A server joined at `at`; the cursor and the credits stay with the servers they belonged to.
    pub(super) fn insert_at(&mut self, at: usize) {
        self.weights.flush(&mut self.credit);
        let before = self.credit.len();
        self.credit.insert(at, 0);
        if self.next < before && at <= self.next {
            self.next += 1;
        }
        self.next %= self.credit.len();
    }

    /// The server at `at` left, `remaining` servers are left.
    pub(super) fn remove_at(&mut self, at: usize, remaining: usize) {
        self.weights.flush(&mut self.credit);
        let before = self.credit.len();
        self.credit.remove(at);
        if self.next < before && at < self.next {
            self.next -= 1;
        }
        self.next = self.next.checked_rem(remaining).unwrap_or(0);
    }

    /// The server at `from` moved to `to`, counted once it has left its place.
    pub(super) fn move_to(&mut self, from: usize, to: usize) {
        self.weights.flush(&mut self.credit);
        let credit = self.credit.remove(from);
        self.credit.insert(to, credit);
        if self.next == from {
            self.next = to;
            return;
        }
        if self.next > from {
            self.next -= 1;
        }
        if self.next >= to {
            self.next += 1;
        }
    }

    pub(super) fn next_random(&mut self) -> u64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        self.random
    }
}

/// Compatibility name retained for monitoring and topology consumers during the pool transition.
pub type RoundRobinPool = EndpointPool;

#[derive(Debug)]
pub struct EndpointLease {
    pub(super) inner: Arc<EndpointLeaseInner>,
}

#[derive(Clone)]
pub(crate) struct EndpointObservation {
    health: Arc<PoolHealthState>,
    server: Arc<PoolEndpoint>,
}

impl EndpointObservation {
    pub(crate) fn record_passive_failure(&self, failure: HealthFailure) {
        self.health.record_passive_failure(&self.server, failure);
    }

    pub(crate) fn record_passive_success(&self) {
        self.health.record_passive_success(&self.server);
    }

    /// Feeds the time to first byte of a successful response on `route`.
    pub(crate) fn record_latency(&self, route: &str, elapsed: Duration) {
        self.health.record_latency(&self.server, route, elapsed);
    }
}

#[derive(Debug)]
pub(super) struct EndpointLeaseInner {
    acquired: AtomicBool,
    admission: Admission,
    deadline: Option<Instant>,
    health: Arc<PoolHealthState>,
    queue: Arc<PoolQueue>,
    server: Arc<PoolEndpoint>,
    wait: LeaseWait,
    /// Set while this lease's pooled connection waits idle for a request to reuse it.
    idle: AtomicBool,
    /// Set while this lease holds a claim on an idle pooled connection of its server.
    reserved: AtomicBool,
    /// Set while this lease holds a place in the queue ledger, so a lease that never queued takes no lock to leave.
    queued: AtomicBool,
}

/// A lease that has to wait for capacity counts as queued from its first wait until it is acquired or dropped.
#[derive(Debug, Default)]
struct LeaseWait {
    observer: Mutex<Option<Arc<AtomicU64>>>,
    ticket: Mutex<Option<WaitTicket>>,
    waited_ms: AtomicU64,
    position: AtomicU64,
}

impl EndpointLease {
    pub(super) fn acquired(
        server: Arc<PoolEndpoint>,
        queue: Arc<PoolQueue>,
        health: Arc<PoolHealthState>,
    ) -> Self {
        Self {
            inner: Arc::new(EndpointLeaseInner {
                acquired: AtomicBool::new(true),
                admission: Admission::Selectable,
                deadline: None,
                health,
                queue,
                server,
                wait: LeaseWait::default(),
                idle: AtomicBool::new(false),
                reserved: AtomicBool::new(false),
                queued: AtomicBool::new(false),
            }),
        }
    }

    pub(super) fn pending(
        server: Arc<PoolEndpoint>,
        queue: Arc<PoolQueue>,
        queue_timeout: Option<std::time::Duration>,
        health: Arc<PoolHealthState>,
        admission: Admission,
    ) -> Self {
        Self {
            inner: Arc::new(EndpointLeaseInner {
                acquired: AtomicBool::new(false),
                admission,
                deadline: queue_timeout.map(|timeout| Instant::now() + timeout),
                health,
                queue,
                server,
                wait: LeaseWait::default(),
                idle: AtomicBool::new(false),
                reserved: AtomicBool::new(false),
                queued: AtomicBool::new(false),
            }),
        }
    }

    /// Marks the lease as holding the claim [`PoolEndpoint::reserve_idle`] just took.
    pub(super) fn reserved(self) -> Self {
        self.inner.reserved.store(true, Ordering::Release);
        self
    }

    #[must_use]
    pub fn endpoint(&self) -> &RuntimeEndpoint {
        &self.inner.server.endpoint
    }

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.inner.server.name
    }

    /// The server's name, shared with the records that mention it.
    pub(crate) fn server_label(&self) -> &crate::label::Label {
        &self.inner.server.label
    }

    /// The server's lifetime counters.
    pub(crate) fn server_stats(&self) -> &Arc<ServerStats> {
        &self.inner.server.stats
    }

    /// Reports this lease's place in the pool queue to `cell` while it waits for capacity.
    ///
    /// Only a lease with a queue deadline ever waits, so any other one reports nothing.
    pub(crate) fn observe_queue_position(&self, cell: &Arc<AtomicU64>) {
        if self.inner.deadline.is_none() {
            return;
        }
        *self
            .inner
            .wait
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(cell));
    }

    /// Records where a selector wait started, for leases that were not bound before waiting.
    pub(super) fn note_selector_position(&self, position: u64) {
        self.inner.wait.position.store(position, Ordering::Relaxed);
    }

    /// The request has its connection: a reused one never acquires this lease, so the wait ends here.
    pub(crate) fn end_queue_wait(&self) {
        self.inner.leave_queue(false);
        self.inner.release_reservation();
    }

    /// Time this lease spent waiting for capacity so far.
    pub(crate) fn queue_wait(&self) -> Duration {
        Duration::from_millis(self.inner.wait.waited_ms.load(Ordering::Relaxed))
    }

    /// How many requests were waiting when this one joined, itself included; 0 if it never waited.
    pub(crate) fn queue_position(&self) -> u64 {
        self.inner.wait.position.load(Ordering::Relaxed)
    }

    /// See `PoolEndpoint::reuse_peer`.
    pub(crate) fn reuse_peer(&self) -> Option<&ReusePeer> {
        self.inner.server.reuse_peer()
    }

    pub(crate) fn observation(&self) -> EndpointObservation {
        EndpointObservation {
            health: Arc::clone(&self.inner.health),
            server: Arc::clone(&self.inner.server),
        }
    }

    pub(crate) fn record_passive_failure(&self, failure: HealthFailure) {
        self.inner
            .health
            .record_passive_failure(&self.inner.server, failure);
    }

    pub(crate) fn record_passive_success(&self) {
        self.inner.health.record_passive_success(&self.inner.server);
    }

    pub(crate) async fn resolve_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        self.inner.server.resolve_addresses().await
    }

    pub(crate) fn connection_lifetime(
        &self,
    ) -> std::sync::Weak<dyn pingora::protocols::ConnectionLifetime> {
        let lifetime: Arc<dyn pingora::protocols::ConnectionLifetime> = self.inner.clone();
        Arc::downgrade(&lifetime)
    }
}

impl EndpointLeaseInner {
    fn ticket(&self) -> std::sync::MutexGuard<'_, Option<WaitTicket>> {
        self.wait
            .ticket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn observe_position(&self, position: u64) {
        let observer = self
            .wait
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cell) = observer.as_ref() {
            cell.store(position, Ordering::Relaxed);
        }
    }

    fn join_queue(&self) {
        let mut ticket = self.ticket();
        if ticket.is_some() {
            return;
        }
        let joined = self.queue.ledger.join();
        self.wait.position.store(joined.position, Ordering::Relaxed);
        self.observe_position(joined.position);
        self.queue.queued.fetch_add(1, Ordering::Relaxed);
        self.queue.queued_total.fetch_add(1, Ordering::Relaxed);
        *ticket = Some(joined);
        self.queued.store(true, Ordering::Release);
    }

    fn leave_queue(&self, cancelled: bool) {
        if !self.queued.load(Ordering::Acquire) {
            return;
        }
        let Some(ticket) = self.ticket().take() else {
            return;
        };
        self.queued.store(false, Ordering::Release);
        let waited = self.queue.ledger.leave(ticket);
        self.observe_position(0);
        decrement_queue_count(&self.queue.queued);
        if cancelled {
            self.queue.cancellations.fetch_add(1, Ordering::Relaxed);
        }
        let millis = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
        self.wait.waited_ms.fetch_add(millis, Ordering::Relaxed);
        crate::observability::upstream_stats::upstream_stats()
            .pool(&self.health.pool_name)
            .record_queue_wait(waited);
    }

    /// Gives back the claim on an idle connection, if the lease holds one; true when it did.
    fn release_reservation(&self) -> bool {
        let held =
            self.reserved.load(Ordering::Acquire) && self.reserved.swap(false, Ordering::AcqRel);
        if held {
            self.server.release_idle(&self.queue);
        }
        held
    }

    fn set_idle(&self, idle: bool) {
        // A lease that is not idle and stays so (the usual case when it is dropped) takes no atomic exchange.
        if self.idle.load(Ordering::Acquire) != idle
            && self.idle.swap(idle, Ordering::AcqRel) != idle
        {
            self.server.set_connection_idle(&self.queue, idle);
        }
    }
}

impl Drop for EndpointLeaseInner {
    fn drop(&mut self) {
        self.set_idle(false);
        self.leave_queue(true);
        if self.release_reservation() {
            self.queue.notify_capacity_waiters();
        }
        if !self.acquired.load(Ordering::Acquire) {
            return;
        }
        self.server.release_capacity(&self.queue);
    }
}

#[async_trait::async_trait]
impl pingora::protocols::ConnectionLifetime for EndpointLeaseInner {
    fn try_acquire(&self) -> pingora::Result<bool> {
        if self.acquired.load(Ordering::Acquire) {
            return Ok(true);
        }
        if self
            .server
            .try_acquire_capacity(&self.queue, self.admission, now_unix_ms())
            .is_none()
        {
            return Ok(false);
        }
        self.acquired.store(true, Ordering::Release);
        self.leave_queue(false);
        if self.release_reservation() {
            self.queue.notify_capacity_waiters();
        }
        Ok(true)
    }

    fn capacity_generation(&self) -> u64 {
        self.queue.generation.load(Ordering::Acquire)
    }

    async fn wait_for_capacity(&self, generation: u64) -> pingora::Result<()> {
        let Some(deadline) = self.deadline else {
            return Err(pingora::Error::new_up(pingora::ErrorType::HTTPStatus(503)));
        };
        self.join_queue();
        #[cfg(test)]
        let _waiting = LifetimeWaitGuard::new(&self.queue.lifetime_waiters);
        let _watch = self.queue.watch();
        loop {
            if self.queue.generation.load(Ordering::Acquire) != generation {
                return Ok(());
            }
            let notified = self.queue.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.queue.generation.load(Ordering::Acquire) != generation {
                return Ok(());
            }
            if timeout_at(deadline, notified).await.is_err() {
                self.queue.timeouts.fetch_add(1, Ordering::Relaxed);
                self.leave_queue(false);
                return Err(pingora::Error::new_up(pingora::ErrorType::HTTPStatus(503)));
            }
        }
    }

    fn notify_reusable(&self) {
        self.set_idle(true);
        if self.queue.capacity_waiters_possible {
            self.queue.notify_capacity_waiters();
        }
    }

    fn notify_in_use(&self) {
        self.set_idle(false);
    }
}

#[cfg(test)]
struct LifetimeWaitGuard<'a> {
    waiters: &'a AtomicU64,
}

#[cfg(test)]
impl<'a> LifetimeWaitGuard<'a> {
    fn new(waiters: &'a AtomicU64) -> Self {
        waiters.fetch_add(1, Ordering::Relaxed);
        Self { waiters }
    }
}

#[cfg(test)]
impl Drop for LifetimeWaitGuard<'_> {
    fn drop(&mut self) {
        let released = self
            .waiters
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |waiters| {
                waiters.checked_sub(1)
            });
        debug_assert!(released.is_ok(), "lifetime waiter counter underflow");
    }
}

pub(super) struct SelectionAttempt {
    pub(super) lease: Option<EndpointLease>,
    pub(super) pool_available: bool,
    pub(super) saturated: bool,
}
