#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug)]
pub(super) struct PoolQueue {
    // The pool's immutable queue timeout owns both selector and connector lifetime waiting.
    pub(super) capacity_waiters_possible: bool,
    pub(super) cancellations: AtomicU64,
    pub(super) generation: AtomicU64,
    pub(super) notify: Notify,
    #[cfg(test)]
    pub(super) notifications: AtomicU64,
    pub(super) queued: AtomicU64,
    pub(super) queued_total: AtomicU64,
    #[cfg(test)]
    pub(super) lifetime_waiters: AtomicU64,
    pub(super) timeouts: AtomicU64,
    waiters: Mutex<VecDeque<Arc<PoolWaiter>>>,
}

#[derive(Debug)]
pub(super) struct PoolWaiter {
    pub(super) notify: Notify,
}

impl PoolQueue {
    pub(super) fn new(capacity_waiters_possible: bool) -> Self {
        Self {
            capacity_waiters_possible,
            cancellations: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            notify: Notify::new(),
            #[cfg(test)]
            notifications: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            queued_total: AtomicU64::new(0),
            #[cfg(test)]
            lifetime_waiters: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            waiters: Mutex::new(VecDeque::new()),
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
            .front()
            .cloned();
        if let Some(waiter) = waiter {
            waiter.notify.notify_one();
        }
    }
}

pub(super) struct QueueWaitGuard {
    completed: bool,
    queue: Arc<PoolQueue>,
    pub(super) waiter: Arc<PoolWaiter>,
    waiting: bool,
}

impl QueueWaitGuard {
    pub(super) fn new(queue: Arc<PoolQueue>) -> Self {
        let waiter = Arc::new(PoolWaiter {
            notify: Notify::new(),
        });
        queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Arc::clone(&waiter));
        Self {
            completed: false,
            queue,
            waiter,
            waiting: false,
        }
    }

    pub(super) fn is_front(&self) -> bool {
        self.queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .front()
            .is_some_and(|waiter| Arc::ptr_eq(waiter, &self.waiter))
    }

    pub(super) fn mark_waiting(&mut self) {
        if self.waiting {
            return;
        }
        self.waiting = true;
        self.queue.queued.fetch_add(1, Ordering::Relaxed);
        self.queue.queued_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn complete(&mut self) {
        self.finish(false);
    }

    pub(super) fn timeout(&mut self) {
        self.finish(true);
    }

    fn finish(&mut self, timed_out: bool) {
        let removed_front = self.remove();
        if self.waiting {
            decrement_queue_count(&self.queue.queued);
        }
        if timed_out {
            self.queue.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        self.completed = true;
        if removed_front {
            self.queue.notify_front();
        }
    }

    fn remove(&self) -> bool {
        let mut waiters = self
            .queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = waiters
            .iter()
            .position(|waiter| Arc::ptr_eq(waiter, &self.waiter))
        else {
            return false;
        };
        let removed_front = index == 0;
        waiters.remove(index);
        removed_front
    }
}

impl Drop for QueueWaitGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let removed_front = self.remove();
        if self.waiting {
            decrement_queue_count(&self.queue.queued);
            self.queue.cancellations.fetch_add(1, Ordering::Relaxed);
        }
        if removed_front {
            self.queue.notify_front();
        }
    }
}

pub(super) fn decrement_queue_count(counter: &AtomicU64) {
    let released = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
        queued.checked_sub(1)
    });
    debug_assert!(released.is_ok(), "pool queue counter underflow");
}
