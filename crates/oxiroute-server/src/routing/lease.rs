#[allow(clippy::wildcard_imports)]
use super::*;

/// A health-aware selector over a fixed, nonempty named-server list.
#[derive(Debug)]
pub struct EndpointPool {
    pub(super) algorithm: UpstreamAlgorithm,
    pub(super) name: String,
    pub(super) endpoints: Arc<[Arc<PoolEndpoint>]>,
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
    /// Smooth weighted round robin: each server's accumulated credit.
    pub(super) credit: Vec<i64>,
    random: u64,
}

impl SelectionState {
    pub(super) fn new(servers: usize) -> Self {
        Self {
            next: 0,
            credit: vec![0; servers],
            random: ejection_entropy(0) | 1,
        }
    }

    pub(super) fn next_random(&mut self) -> u64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        self.random
    }
}

/// A selectable server and what the algorithms rank it by.
#[derive(Clone, Copy, Debug)]
pub(super) struct Candidate {
    pub(super) index: usize,
    pub(super) load: u64,
    pub(super) weight: u16,
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
            }),
        }
    }

    #[must_use]
    pub fn endpoint(&self) -> &RuntimeEndpoint {
        &self.inner.server.endpoint
    }

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.inner.server.name
    }

    /// Reports this lease's place in the pool queue to `cell` while it waits for capacity.
    pub(crate) fn observe_queue_position(&self, cell: Arc<AtomicU64>) {
        *self
            .inner
            .wait
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cell);
    }

    /// Records where a selector wait started, for leases that were not bound before waiting.
    pub(super) fn note_selector_position(&self, position: u64) {
        self.inner.wait.position.store(position, Ordering::Relaxed);
    }

    /// The request has its connection: a reused one never acquires this lease, so the wait ends here.
    pub(crate) fn end_queue_wait(&self) {
        self.inner.leave_queue(false);
    }

    /// Time this lease spent waiting for capacity so far.
    pub(crate) fn queue_wait(&self) -> Duration {
        Duration::from_millis(self.inner.wait.waited_ms.load(Ordering::Relaxed))
    }

    /// How many requests were waiting when this one joined, itself included; 0 if it never waited.
    pub(crate) fn queue_position(&self) -> u64 {
        self.inner.wait.position.load(Ordering::Relaxed)
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
    }

    fn leave_queue(&self, cancelled: bool) {
        let Some(ticket) = self.ticket().take() else {
            return;
        };
        let waited = self.queue.ledger.leave(ticket);
        self.observe_position(0);
        decrement_queue_count(&self.queue.queued);
        if cancelled {
            self.queue.cancellations.fetch_add(1, Ordering::Relaxed);
        }
        let millis = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
        self.wait.waited_ms.fetch_add(millis, Ordering::Relaxed);
        crate::upstream_stats::upstream_stats()
            .pool(&self.health.pool_name)
            .record_queue_wait(waited);
    }

    fn set_idle(&self, idle: bool) {
        if self.idle.swap(idle, Ordering::AcqRel) != idle {
            if idle {
                self.server.idle_permits.fetch_add(1, Ordering::AcqRel);
            } else {
                self.server.idle_permits.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

impl Drop for EndpointLeaseInner {
    fn drop(&mut self) {
        self.set_idle(false);
        self.leave_queue(true);
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
            .try_acquire_capacity(self.admission, now_unix_ms())
            .is_none()
        {
            return Ok(false);
        }
        self.acquired.store(true, Ordering::Release);
        self.leave_queue(false);
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
