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
    #[cfg(test)]
    pub(super) notifications: AtomicU64,
    pub(super) queued: AtomicU64,
    pub(super) queued_total: AtomicU64,
    #[cfg(test)]
    pub(super) lifetime_waiters: AtomicU64,
    pub(super) timeouts: AtomicU64,
    waiters: Mutex<WaitQueue>,
    pub(super) schedule: Option<Arc<QueueSchedule>>,
}

#[derive(Debug)]
pub(super) struct PoolWaiter {
    pub(super) notify: Notify,
}

impl PoolQueue {
    pub(super) fn new(
        capacity_waiters_possible: bool,
        schedule: Option<Arc<QueueSchedule>>,
    ) -> Self {
        Self {
            capacity_waiters_possible,
            cancellations: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            ledger: WaitLedger::default(),
            notify: Notify::new(),
            #[cfg(test)]
            notifications: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            queued_total: AtomicU64::new(0),
            #[cfg(test)]
            lifetime_waiters: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            waiters: Mutex::new(WaitQueue::new(schedule.clone())),
            schedule,
        }
    }

    pub(super) fn notify_capacity_waiters(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        #[cfg(test)]
        self.notifications.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_waiters();
        self.notify_front();
    }

    fn notify_front(&self) {
        let waiter = self
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .head()
            .cloned();
        if let Some(waiter) = waiter {
            waiter.notify.notify_one();
        }
    }
}

pub(super) struct QueueWaitGuard {
    completed: bool,
    queue: Arc<PoolQueue>,
    stats: Arc<crate::upstream_stats::PoolStats>,
    ticket: Option<WaitTicket>,
    pub(super) waiter: Arc<PoolWaiter>,
    waiting: bool,
}

impl QueueWaitGuard {
    pub(super) fn new(queue: Arc<PoolQueue>, pool: &str, tag: QueueTag) -> Self {
        let waiter = Arc::new(PoolWaiter {
            notify: Notify::new(),
        });
        queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::clone(&waiter), tag);
        Self {
            completed: false,
            queue,
            stats: crate::upstream_stats::upstream_stats().pool(pool),
            ticket: None,
            waiter,
            waiting: false,
        }
    }

    pub(super) fn is_front(&self) -> bool {
        self.queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_head(&self.waiter)
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
            self.stats
                .record_queue_wait(self.queue.ledger.leave(ticket));
        }
    }

    fn remove(&self, served: bool) -> bool {
        self.queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.waiter, served)
    }
}

impl Drop for QueueWaitGuard {
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
        let depth = self
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .depth_by_class();
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
