//! Deadlines of the HTTP/1 lane that cost almost nothing when the work they bound has to wait.
//!
//! A timer is registered with the runtime's timer wheel the first time its work cannot go on, as in [`super::timing`],
//! but it is not created and dropped for every wait: a thread keeps the timers it used last, by kind, and a wait takes
//! one back and moves it to its new deadline. Moving a registered timer to a later deadline is one atomic operation
//! and no wheel lock (the wheel finds the timer early and puts it back), and the waits of one kind have the same
//! length, so their deadlines only move forward. A timer that is back in the pool holds no waker.

use std::{
    cell::RefCell,
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
    time::Duration,
};

use tokio::time::{Instant, Sleep, sleep_until};

use super::timing::Expired;

/// The kind of wait a timer serves; the pool keeps them apart so a timer sees deadlines of one length.
#[derive(Clone, Copy)]
pub(in crate::http::proxy) enum Wait {
    /// A connection waiting for its next request.
    Idle,
    /// One step of an exchange with the origin.
    Step,
    /// The whole response.
    Total,
}

const KINDS: usize = 3;
/// Timers a thread keeps per kind: waits nest only as far as one exchange.
const POOLED: usize = 4;

thread_local! {
    static SLEEPS: RefCell<[Vec<Pin<Box<Sleep>>>; KINDS]> = const { RefCell::new([Vec::new(), Vec::new(), Vec::new()]) };
}

/// A timer on loan from the pool of the thread; it goes back, with no waker, when the wait is over.
struct Loan {
    wait: Wait,
    sleep: Option<Pin<Box<Sleep>>>,
}

impl Loan {
    fn new(wait: Wait, deadline: Instant) -> Self {
        let pooled = SLEEPS.with(|pool| pool.borrow_mut()[wait as usize].pop());
        let sleep = match pooled {
            Some(mut sleep) => {
                sleep.as_mut().reset(deadline);
                sleep
            }
            None => Box::pin(sleep_until(deadline)),
        };
        Self {
            wait,
            sleep: Some(sleep),
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.sleep
            .as_mut()
            .expect("a loaned timer")
            .as_mut()
            .poll(cx)
    }
}

impl Drop for Loan {
    fn drop(&mut self) {
        let Some(mut sleep) = self.sleep.take() else {
            return;
        };
        // The task that waited may be gone by the time the timer fires: the timer must not keep it alive.
        let _ = sleep.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        let _ = SLEEPS.try_with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool[self.wait as usize].len() < POOLED {
                pool[self.wait as usize].push(sleep);
            }
        });
    }
}

/// Runs `work` and gives up at `deadline`, like [`super::timing::within`], with a pooled timer.
pub(in crate::http::proxy) async fn within<F: Future>(
    wait: Wait,
    deadline: Instant,
    work: F,
) -> Result<F::Output, Expired> {
    within_at(wait, move || deadline, work).await
}

/// Runs `work` for at most `limit` (no limit when `None`), like [`super::timing::within_for`], with a pooled timer.
/// The limit starts when the work first has to wait.
pub(in crate::http::proxy) async fn within_for<F: Future>(
    wait: Wait,
    limit: Option<Duration>,
    work: F,
) -> Result<F::Output, Expired> {
    let Some(limit) = limit else {
        return Ok(work.await);
    };
    within_at(wait, move || Instant::now() + limit, work).await
}

async fn within_at<F: Future>(
    wait: Wait,
    deadline: impl FnOnce() -> Instant,
    work: F,
) -> Result<F::Output, Expired> {
    let mut work = pin!(work);
    let (mut deadline, mut timer) = (Some(deadline), None::<Loan>);
    poll_fn(|cx| {
        if let Poll::Ready(output) = work.as_mut().poll(cx) {
            return Poll::Ready(Ok(output));
        }
        let timer =
            timer.get_or_insert_with(|| Loan::new(wait, (deadline.take().expect("armed once"))()));
        timer.poll(cx).map(|()| Err(Expired))
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn work_that_finishes_is_returned_and_one_that_waits_past_the_deadline_expires() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(within(Wait::Step, deadline, async { 7 }).await.unwrap(), 7);
        let slow = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            8
        };
        assert_eq!(within(Wait::Step, deadline, slow).await.unwrap(), 8);
        let stuck = tokio::time::sleep(Duration::from_mins(1));
        assert!(within(Wait::Step, deadline, stuck).await.is_err());
        assert!(Instant::now() >= deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn a_timer_taken_from_the_pool_obeys_its_new_deadline_earlier_or_later() {
        for seconds in [10, 30, 5, 20, 1] {
            let limit = Some(Duration::from_secs(seconds));
            let start = Instant::now();
            let stuck = tokio::time::sleep(Duration::from_mins(5));
            assert!(within_for(Wait::Idle, limit, stuck).await.is_err());
            assert_eq!(start.elapsed(), Duration::from_secs(seconds));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn waits_of_one_exchange_nest_and_each_keeps_its_own_limit() {
        let outer = within_for(Wait::Total, Some(Duration::from_secs(10)), async {
            let first = within_for(
                Wait::Step,
                Some(Duration::from_secs(2)),
                tokio::time::sleep(Duration::from_secs(1)),
            )
            .await;
            let second = within_for(
                Wait::Step,
                Some(Duration::from_secs(2)),
                tokio::time::sleep(Duration::from_secs(3)),
            )
            .await;
            (first.is_ok(), second.is_ok())
        })
        .await;
        assert_eq!(outer.unwrap(), (true, false));
    }

    #[tokio::test(start_paused = true)]
    async fn no_limit_waits_for_the_work() {
        let unbounded = tokio::time::sleep(Duration::from_secs(5));
        assert!(within_for(Wait::Step, None, unbounded).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_returned_timer_does_not_keep_the_waiting_task_alive() {
        struct Task(std::sync::atomic::AtomicUsize);
        impl std::task::Wake for Task {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let task = std::sync::Arc::new(Task(0.into()));
        let waker = Waker::from(std::sync::Arc::clone(&task));
        let mut cx = Context::from_waker(&waker);
        let mut wait = Box::pin(within_for(
            Wait::Total,
            Some(Duration::from_mins(1)),
            tokio::time::sleep(Duration::from_secs(1)),
        ));
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(wait.as_mut().poll(&mut cx).is_ready());
        drop(wait);
        drop(waker);
        assert_eq!(
            std::sync::Arc::strong_count(&task),
            1,
            "the pooled timer still holds the waker of the task"
        );
    }
}
