//! A task executor for one worker thread whose ready queue is neither first-in first-out nor last-in first-out.
//!
//! The newest ready task runs first, because its state and its socket are the most likely to still be in the CPU
//! caches. A task that has waited for the [`Age`] runs before any newer one, so no task waits longer than the age
//! plus the time to serve the tasks that are already older than the age. The age is a duration (a count of polls
//! would stretch with the load), either fixed or a multiple of the time one pass over the ready tasks takes.
//!
//! [`AgedExecutor::new`] returns a handle that any thread can [`spawn`](AgedExecutor::spawn) on, and a [`Runner`],
//! the future that polls the tasks. The runner is meant to be spawned once on the worker's `current_thread` Tokio
//! runtime, so the timers, the I/O driver and the wakers of the tasks work as in any Tokio task. Dropping the runner
//! drops every task it owns.

mod migrate;
mod queue;
mod runner;

pub use migrate::{in_task, move_to};
pub use queue::{Age, Stats};
pub use runner::Runner;

use std::{
    future::Future,
    sync::{Arc, Mutex, Weak},
};

use queue::Shared;

/// Handle used to submit tasks to the executor of one worker.
#[derive(Clone)]
pub struct AgedExecutor {
    shared: Arc<Shared>,
}

static EXECUTORS: Mutex<Vec<Weak<Shared>>> = Mutex::new(Vec::new());

impl AgedExecutor {
    /// An executor whose ready tasks wait at most `age` behind newer ones, with the future that runs it.
    #[must_use]
    pub fn new(age: Age) -> (Self, Runner) {
        Self::build(age, false)
    }

    /// [`Self::new`] whose runner also times its polls, for [`Self::busy_nanos`]: two clock reads per batch of tasks.
    #[must_use]
    pub fn new_measured(age: Age) -> (Self, Runner) {
        Self::build(age, true)
    }

    fn build(age: Age, measured: bool) -> (Self, Runner) {
        let shared = Arc::new(Shared::new(age, measured));
        let mut executors = queue::lock(&EXECUTORS);
        executors.retain(|executor| executor.strong_count() > 0);
        executors.push(Arc::downgrade(&shared));
        (
            Self {
                shared: Arc::clone(&shared),
            },
            Runner::new(shared),
        )
    }

    /// Queues `future` to run on this executor. It is dropped at once when the runner is gone.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.shared.submit(Box::pin(future));
    }

    pub(crate) fn submit_task(&self, task: queue::Task) {
        self.shared.submit(task);
    }

    /// Nanoseconds the runner has spent polling tasks since it started; zero unless the executor is measured. Only the
    /// difference between two readings says anything.
    #[must_use]
    pub fn busy_nanos(&self) -> u64 {
        self.shared.busy_nanos()
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        self.shared.stats()
    }

    /// The sum of the statistics of every live executor of the process (`oldest_ready` and `age` are the largest).
    #[must_use]
    pub fn process_stats() -> Stats {
        let executors = queue::lock(&EXECUTORS);
        executors
            .iter()
            .filter_map(Weak::upgrade)
            .map(|shared| shared.stats())
            .fold(Stats::default(), Stats::combine)
    }
}
