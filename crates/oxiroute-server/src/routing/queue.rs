#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug)]
pub(super) struct PoolQueue {
    // The pool's immutable queue timeout owns both selector and connector lifetime waiting.
    pub(super) capacity_waiters_possible: bool,
    pub(super) cancellations: AtomicU64,
    pub(super) generation: AtomicU64,
    pub(super) ledger: WaitLedger,
    pub(super) notify: Notify,
    /// Tasks that may be asleep on `notify`; a release wakes the pool only when there is one.
    watchers: AtomicUsize,
    #[cfg(test)]
    pub(super) notifications: AtomicU64,
    #[cfg(test)]
    pub(super) enqueued: AtomicU64,
    pub(super) queued: AtomicU64,
    pub(super) queued_total: AtomicU64,
    #[cfg(test)]
    pub(super) lifetime_waiters: AtomicU64,
    pub(super) timeouts: AtomicU64,
    /// Length of `waiters`, readable without its lock: capacity events and new arrivals only
    /// involve the queue while somebody is in it.
    selectors: AtomicUsize,
    waiters: Mutex<WaitQueue>,
    pub(super) schedule: Option<Arc<QueueSchedule>>,
    /// Servers whose load changed, for the algorithms that pick by load.
    pub(super) load_log: LoadLog,
}

pub(super) struct CapacityWatch<'a> {
    queue: &'a PoolQueue,
}

impl Drop for CapacityWatch<'_> {
    fn drop(&mut self) {
        self.queue.watchers.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Debug)]
pub(super) struct PoolWaiter {
    pub(super) notify: Notify,
}

impl PoolQueue {
    pub(super) fn new(
        capacity_waiters_possible: bool,
        schedule: Option<Arc<QueueSchedule>>,
        load_log: LoadLog,
    ) -> Self {
        Self {
            capacity_waiters_possible,
            cancellations: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            ledger: WaitLedger::default(),
            notify: Notify::new(),
            watchers: AtomicUsize::new(0),
            #[cfg(test)]
            notifications: AtomicU64::new(0),
            #[cfg(test)]
            enqueued: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            queued_total: AtomicU64::new(0),
            #[cfg(test)]
            lifetime_waiters: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            selectors: AtomicUsize::new(0),
            waiters: Mutex::new(WaitQueue::new(schedule.clone())),
            schedule,
            load_log,
        }
    }

    pub(super) fn notify_capacity_waiters(&self) {
        #[cfg(test)]
        self.notifications.fetch_add(1, Ordering::Relaxed);
        if self.capacity_waiters_possible {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        // Pairs with `watch` and with the fence in `enqueue`: a task that registered after this
        // fence reads the state the caller changed before it, so it never sleeps through the event.
        fence(Ordering::SeqCst);
        if self.watchers.load(Ordering::Relaxed) != 0 {
            self.notify.notify_waiters();
        }
        if self.capacity_waiters_possible && self.selectors.load(Ordering::Relaxed) != 0 {
            self.notify_front();
        }
    }

    /// Registers a task about to wait on `notify`; register before reading the state it waits for.
    pub(super) fn watch(&self) -> CapacityWatch<'_> {
        self.watchers.fetch_add(1, Ordering::SeqCst);
        CapacityWatch { queue: self }
    }

    fn notify_front(&self) {
        let waiter = self
            .lock_waiters()
            .head()
            .map(|(_, waiter)| Arc::clone(waiter));
        if let Some(waiter) = waiter {
            waiter.notify.notify_one();
        }
    }

    fn lock_waiters(&self) -> std::sync::MutexGuard<'_, WaitQueue> {
        self.waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether nobody is queued, so an arrival has nobody to wait behind.
    pub(super) fn is_unqueued(&self) -> bool {
        self.selectors.load(Ordering::Relaxed) == 0
    }

    fn enqueue(&self, waiter: Arc<PoolWaiter>, tag: QueueTag) -> u64 {
        #[cfg(test)]
        self.enqueued.fetch_add(1, Ordering::Relaxed);
        let seq = {
            let mut waiters = self.lock_waiters();
            let seq = waiters.push(waiter, tag);
            self.selectors.store(waiters.len(), Ordering::Relaxed);
            seq
        };
        fence(Ordering::SeqCst);
        seq
    }

    /// Removes a waiter and reports whether it was the head.
    fn dequeue(&self, seq: u64, served: bool) -> bool {
        let mut waiters = self.lock_waiters();
        let was_head = waiters.remove(seq, served);
        self.selectors.store(waiters.len(), Ordering::Relaxed);
        was_head
    }

    fn is_head(&self, seq: u64) -> bool {
        self.lock_waiters().is_head(seq)
    }
}

pub(super) struct QueueWaitGuard<'a> {
    completed: bool,
    pool: &'a str,
    queue: Arc<PoolQueue>,
    seq: u64,
    ticket: Option<WaitTicket>,
    pub(super) waiter: Arc<PoolWaiter>,
    waiting: bool,
}

impl<'a> QueueWaitGuard<'a> {
    pub(super) fn new(queue: Arc<PoolQueue>, pool: &'a str, tag: QueueTag) -> Self {
        let waiter = Arc::new(PoolWaiter {
            notify: Notify::new(),
        });
        let seq = queue.enqueue(Arc::clone(&waiter), tag);
        Self {
            completed: false,
            pool,
            queue,
            seq,
            ticket: None,
            waiter,
            waiting: false,
        }
    }

    pub(super) fn is_front(&self) -> bool {
        self.queue.is_head(self.seq)
    }

    pub(super) fn mark_waiting(&mut self) {
        if self.waiting {
            return;
        }
        self.waiting = true;
        self.ticket = Some(self.queue.ledger.join());
        self.queue.queued.fetch_add(1, Ordering::Relaxed);
        self.queue.queued_total.fetch_add(1, Ordering::Relaxed);
    }

    /// How many requests were waiting when this one joined, itself included; 0 if it never waited.
    pub(super) fn position(&self) -> u64 {
        self.ticket.as_ref().map_or(0, |ticket| ticket.position)
    }

    /// Leaves the queue without having been served.
    pub(super) fn complete(&mut self) {
        self.finish(false, false);
    }

    /// Leaves the queue with a lease, which spends the waiter's turn.
    pub(super) fn serve(&mut self) {
        self.finish(false, true);
    }

    pub(super) fn timeout(&mut self) {
        self.finish(true, false);
    }

    fn finish(&mut self, timed_out: bool, served: bool) {
        let removed_front = self.remove(served);
        if self.waiting {
            decrement_queue_count(&self.queue.queued);
            self.leave_ledger();
        }
        if timed_out {
            self.queue.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        self.completed = true;
        if removed_front {
            self.queue.notify_front();
        }
    }

    fn leave_ledger(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            crate::observability::upstream_stats::upstream_stats()
                .pool(self.pool)
                .record_queue_wait(self.queue.ledger.leave(ticket));
        }
    }

    fn remove(&self, served: bool) -> bool {
        self.queue.dequeue(self.seq, served)
    }
}

impl Drop for QueueWaitGuard<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let removed_front = self.remove(false);
        if self.waiting {
            decrement_queue_count(&self.queue.queued);
            self.leave_ledger();
            self.queue.cancellations.fetch_add(1, Ordering::Relaxed);
        }
        if removed_front {
            self.queue.notify_front();
        }
    }
}

impl PoolQueue {
    /// Waiting requests per class name; empty when the pool has no queue classes.
    pub(super) fn depth_by_class(&self) -> Vec<(String, u64)> {
        let Some(schedule) = &self.schedule else {
            return Vec::new();
        };
        let depth = self.lock_waiters().depth_by_class();
        schedule
            .class_names()
            .map(str::to_owned)
            .zip(depth)
            .collect()
    }
}

pub(super) fn decrement_queue_count(counter: &AtomicU64) {
    let released = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
        queued.checked_sub(1)
    });
    debug_assert!(released.is_ok(), "pool queue counter underflow");
}
