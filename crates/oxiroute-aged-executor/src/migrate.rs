//! Moving a running task to another worker.
//!
//! A task that sits at a point where it holds nothing tied to its worker (a connection between two requests) calls
//! [`move_to`]: the runner that polls it takes its future out of its slab and queues it on the target executor, so the
//! task continues there with all of its state.

use std::{cell::RefCell, future::poll_fn, task::Poll};

use crate::{AgedExecutor, queue::Task};

thread_local! {
    /// Set by the runner while it polls a task: whether one is being polled, and the move the task asked for.
    static POLLING: RefCell<(bool, Option<AgedExecutor>)> = const { RefCell::new((false, None)) };
}

/// Runs a poll of a task, so that [`in_task`] and [`move_to`] know a runner is behind them.
pub(crate) fn polling<T>(poll: impl FnOnce() -> T) -> T {
    POLLING.with_borrow_mut(|state| state.0 = true);
    let polled = poll();
    POLLING.with_borrow_mut(|state| state.0 = false);
    polled
}

pub(crate) fn take_request() -> Option<Target> {
    POLLING.with_borrow_mut(|state| state.1.take()).map(Target)
}

/// Where a task asked to continue.
pub(crate) struct Target(AgedExecutor);

impl Target {
    pub(crate) fn submit(self, future: Task) {
        self.0.submit_task(future);
    }
}

/// Whether the caller runs as a task of an [`AgedExecutor`].
#[must_use]
pub fn in_task() -> bool {
    POLLING.with_borrow(|state| state.0)
}

/// Continues the calling task on `target`'s worker. True once it runs there; false at once, without waiting, when the
/// caller is not a task of an executor (a Tokio task, say), because only a runner can carry the task over.
///
/// Wakers the task registered before the move do nothing afterwards, so call this only where no sub-future of the task
/// is parked: the task polls everything it awaits again once it continues.
pub async fn move_to(target: AgedExecutor) -> bool {
    if !in_task() {
        return false;
    }
    let mut target = Some(target);
    poll_fn(|_| match target.take() {
        Some(target) => {
            POLLING.with_borrow_mut(|state| state.1 = Some(target));
            Poll::Pending
        }
        None => Poll::Ready(()),
    })
    .await;
    true
}
