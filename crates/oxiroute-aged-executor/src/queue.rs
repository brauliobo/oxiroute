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

/// How long a ready task may wait behind newer ones before it is served first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Age {
    Fixed(Duration),
    /// This many times the time one pass over the ready tasks takes (their number times the recent time per poll,
    /// the driver and everything else on the worker included), between [`Age::MIN`] and [`Age::MAX`]. Newest-first
    /// only pays when the ready tasks are more than the caches hold, and then a pass is long: a fixed age shorter
    /// than a pass turns the queue into first-in first-out, a longer one lets the tail grow with the load.
    Adaptive(f64),
}

impl Age {
    pub const MIN: Duration = Duration::from_millis(1);
    pub const MAX: Duration = Duration::from_secs(1);
}

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
    /// The age after which the oldest ready task is served first.
    pub age: Duration,
}

impl Stats {
    pub(crate) fn combine(self, other: Self) -> Self {
        Self {
            tasks: self.tasks + other.tasks,
            ready: self.ready + other.ready,
            polls: self.polls + other.polls,
            aged_polls: self.aged_polls + other.aged_polls,
            oldest_ready: self.oldest_ready.max(other.oldest_ready),
            age: self.age.max(other.age),
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
    /// The age in force: the configured one, or the adaptive one as of the last batch.
    age: Duration,
    /// Smoothed time per poll in nanoseconds, the start of the previous batch and the polls made until then.
    poll_ns: f64,
    last_batch: Option<(Instant, u64)>,
    /// The previous batch ended because nothing was ready, so the gap since it says nothing about the poll rate.
    idle: bool,
}

impl State {
    /// Smooths the time per poll over the batches that ran back to back and sets the age from the tasks that are
    /// ready now.
    #[allow(clippy::cast_precision_loss)]
    fn adapt(&mut self, multiple: f64, now: Instant) {
        const SMOOTHING: f64 = 8.0;
        if let Some((started, polls)) = self.last_batch.filter(|_| !self.idle) {
            let done = self.polls - polls;
            if done > 0 {
                let sample = now.saturating_duration_since(started).as_nanos() as f64 / done as f64;
                self.poll_ns = if self.poll_ns == 0.0 {
                    sample
                } else {
                    self.poll_ns + (sample - self.poll_ns) / SMOOTHING
                };
            }
        }
        self.idle = false;
        self.last_batch = Some((now, self.polls));
        let ready = (self.ready.len() + self.inbox.len()) as f64;
        self.age = Duration::from_secs_f64(multiple * ready * self.poll_ns / 1e9)
            .clamp(Age::MIN, Age::MAX);
    }
}

pub(crate) struct Shared {
    age: Age,
    state: Mutex<State>,
    runner: AtomicWaker,
}

impl Shared {
    pub(crate) fn new(age: Age) -> Self {
        let state = State {
            age: match age {
                Age::Fixed(age) => age,
                Age::Adaptive(_) => Age::MIN,
            },
            ..State::default()
        };
        Self {
            age,
            state: Mutex::new(state),
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
        if let Age::Adaptive(multiple) = self.age {
            state.adapt(multiple, now);
        }
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
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= state.age);
        let Some((task, _)) = (if aged {
            state.ready.pop_front()
        } else {
            state.ready.pop_back()
        }) else {
            state.idle = true;
            return None;
        };
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
            age: state.age,
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
