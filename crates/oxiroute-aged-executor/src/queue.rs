use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    task::Wake,
    time::{Duration, Instant},
};

use atomic_waker::AtomicWaker;

/// Nothing that holds these locks can leave the data half updated, so a poisoned lock is still consistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Counters and gauges of one executor (or of all of them, see `AgedExecutor::process_stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Tasks spawned and not finished.
    pub tasks: usize,
    /// Tasks that are ready to run now.
    pub ready: usize,
    /// Polls since the start.
    pub polls: u64,
    /// Polls that served the oldest ready task because it had waited for the age.
    pub aged_polls: u64,
    /// How long the oldest ready task has been waiting.
    pub oldest_ready: Duration,
}

impl Stats {
    pub(crate) fn combine(self, other: Self) -> Self {
        Self {
            tasks: self.tasks + other.tasks,
            ready: self.ready + other.ready,
            polls: self.polls + other.polls,
            aged_polls: self.aged_polls + other.aged_polls,
            oldest_ready: self.oldest_ready.max(other.oldest_ready),
        }
    }
}

#[derive(Default)]
struct State {
    inbox: Vec<Task>,
    ready: VecDeque<(Arc<TaskWaker>, Instant)>,
    /// The time ready tasks are stamped with: the start of the current batch, or the moment the queue stopped being
    /// empty. One clock read per batch instead of one per wake-up.
    clock: Option<Instant>,
    tasks: usize,
    polls: u64,
    aged_polls: u64,
    closed: bool,
}

pub(crate) struct Shared {
    max_age: Duration,
    state: Mutex<State>,
    runner: AtomicWaker,
}

impl Shared {
    pub(crate) fn new(max_age: Duration) -> Self {
        Self {
            max_age,
            state: Mutex::new(State::default()),
            runner: AtomicWaker::new(),
        }
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
        state.clock = Some(now);
        std::mem::take(&mut state.inbox)
    }

    pub(crate) fn push_ready(&self, task: Arc<TaskWaker>) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        let clock = match state.clock {
            Some(clock) if !state.ready.is_empty() => clock,
            _ => *state.clock.insert(Instant::now()),
        };
        state.ready.push_back((task, clock));
        drop(state);
        self.runner.wake();
    }

    /// The newest ready task, or the oldest when it has waited for the age.
    pub(crate) fn pop(&self, now: Instant) -> Option<Arc<TaskWaker>> {
        let mut state = self.state();
        let aged = state
            .ready
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= self.max_age);
        let (task, _) = if aged {
            state.ready.pop_front()
        } else {
            state.ready.pop_back()
        }?;
        state.polls += 1;
        state.aged_polls += u64::from(aged);
        Some(task)
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
        (
            std::mem::take(&mut state.inbox),
            std::mem::take(&mut state.ready),
        )
    }

    pub(crate) fn stats(&self) -> Stats {
        let state = self.state();
        Stats {
            tasks: state.tasks,
            ready: state.ready.len(),
            polls: state.polls,
            aged_polls: state.aged_polls,
            oldest_ready: state
                .ready
                .front()
                .map_or(Duration::ZERO, |(_, at)| at.elapsed()),
        }
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
