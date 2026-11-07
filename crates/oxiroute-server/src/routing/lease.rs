#[allow(clippy::wildcard_imports)]
use super::*;

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

#[derive(Debug, Default)]
pub(super) struct SelectionState {
    pub(super) next: usize,
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
}

#[derive(Debug)]
pub(super) struct EndpointLeaseInner {
    acquired: AtomicBool,
    admission: Admission,
    deadline: Option<Instant>,
    health: Arc<PoolHealthState>,
    queue: Arc<PoolQueue>,
    server: Arc<PoolEndpoint>,
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

impl Drop for EndpointLeaseInner {
    fn drop(&mut self) {
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
        Ok(true)
    }

    fn capacity_generation(&self) -> u64 {
        self.queue.generation.load(Ordering::Acquire)
    }

    async fn wait_for_capacity(&self, generation: u64) -> pingora::Result<()> {
        let Some(deadline) = self.deadline else {
            return Err(pingora::Error::new_up(pingora::ErrorType::HTTPStatus(503)));
        };
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
                return Err(pingora::Error::new_up(pingora::ErrorType::HTTPStatus(503)));
            }
        }
    }

    fn notify_reusable(&self) {
        if self.queue.capacity_waiters_possible {
            self.queue.notify_capacity_waiters();
        }
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
