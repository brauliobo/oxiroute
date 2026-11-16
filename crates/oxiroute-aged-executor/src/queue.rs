use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::Wake,
    time::{Duration, Instant},
};

use atomic_waker::AtomicWaker;
pub use oxiroute_aged_order::{Age, Stats};
use oxiroute_aged_order::AgedQueue;

/// Nothing that holds these locks can leave the data half updated, so a poisoned lock is still consistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

struct State {
    inbox: Vec<Task>,
    ready: AgedQueue<Arc<TaskWaker>>,
    tasks: usize,
    closed: bool,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    runner: AtomicWaker,
    /// Whether the runner adds the time it spends polling tasks to `busy_ns`.
    measured: bool,
    busy_ns: AtomicU64,
}

impl Shared {
    pub(crate) fn new(age: Age, measured: bool) -> Self {
        let state = State {
            inbox: Vec::new(),
            ready: AgedQueue::new(age),
            tasks: 0,
            closed: false,
        };
        Self {
            state: Mutex::new(state),
            runner: AtomicWaker::new(),
            measured,
            busy_ns: AtomicU64::new(0),
        }
    }

    pub(crate) fn measured(&self) -> bool {
        self.measured
    }

    /// Only the runner, one thread, adds to it.
    pub(crate) fn add_busy(&self, spent: Duration) {
        let spent = u64::try_from(spent.as_nanos()).unwrap_or(u64::MAX);
        self.busy_ns
            .store(self.busy_nanos().saturating_add(spent), Ordering::Relaxed);
    }

    pub(crate) fn busy_nanos(&self) -> u64 {
        self.busy_ns.load(Ordering::Relaxed)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub(crate) fn submit(&self, task: Task) {
        let mut state = self.state();
        if state.closed {
            drop(state);
            return;
        }
        state.inbox.push(task);
        drop(state);
        self.runner.wake();
    }

    pub(crate) fn register_runner(&self, waker: &std::task::Waker) {
        self.runner.register(waker);
    }

    /// Starts a batch at `now` and takes the tasks submitted since the last one.
    pub(crate) fn begin_batch(&self, now: Instant) -> Vec<Task> {
        let mut state = self.state();
        let submitted = state.inbox.len();
        state.ready.begin_batch(now, submitted);
        std::mem::take(&mut state.inbox)
    }

    pub(crate) fn push_ready(&self, task: Arc<TaskWaker>) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.ready.push(task);
        drop(state);
        self.runner.wake();
    }

    /// The newest ready task, or the oldest when it has waited for the age.
    pub(crate) fn pop(&self, now: Instant) -> Option<Arc<TaskWaker>> {
        self.state().ready.pop(now)
    }

    pub(crate) fn has_ready(&self) -> bool {
        !self.state().ready.is_empty()
    }

    pub(crate) fn count_tasks(&self, spawned: usize, finished: usize) {
        let mut state = self.state();
        state.tasks = state.tasks + spawned - finished;
    }

    /// Stops accepting tasks and returns what is queued, to be dropped outside the lock.
    pub(crate) fn close(&self) -> (Vec<Task>, VecDeque<(Arc<TaskWaker>, Instant)>) {
        let mut state = self.state();
        state.closed = true;
        (std::mem::take(&mut state.inbox), state.ready.drain())
    }

    pub(crate) fn stats(&self) -> Stats {
        let state = self.state();
        state.ready.stats(state.tasks)
    }
}

/// The waker of one task: it queues the task on its executor unless it is queued already.
pub(crate) struct TaskWaker {
    pub(crate) id: usize,
    /// True while the task sits in the ready queue, and for good once it has finished.
    pub(crate) queued: AtomicBool,
    shared: Arc<Shared>,
}

impl TaskWaker {
    pub(crate) fn new(id: usize, shared: Arc<Shared>) -> Self {
        Self {
            id,
            queued: AtomicBool::new(false),
            shared,
        }
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.queued.swap(true, Ordering::AcqRel) {
            self.shared.push_ready(Arc::clone(self));
        }
    }
}

#[cfg(test)]
mod tests;
