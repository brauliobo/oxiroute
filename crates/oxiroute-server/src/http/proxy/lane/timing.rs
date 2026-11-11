//! A deadline that costs a timer only when the work it bounds has to wait.

use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::Poll,
    time::Duration,
};

use tokio::time::{Instant, Sleep, sleep, sleep_until};

/// The work did not finish by its deadline.
#[derive(Debug)]
pub(in crate::http::proxy) struct Expired;

/// Runs `work` and gives up at `deadline`. The timer is registered the first time `work` cannot go on, so work that
/// completes on its first poll (a response that fits the socket buffers) never touches the timer wheel.
pub(in crate::http::proxy) async fn within<F: Future>(
    deadline: Instant,
    work: F,
) -> Result<F::Output, Expired> {
    let mut work = pin!(work);
    let mut timer: Option<Pin<Box<Sleep>>> = None;
    poll_fn(|cx| {
        if let Poll::Ready(output) = work.as_mut().poll(cx) {
            return Poll::Ready(Ok(output));
        }
        let timer = timer.get_or_insert_with(|| Box::pin(sleep_until(deadline)));
        timer.as_mut().poll(cx).map(|()| Err(Expired))
    })
    .await
}

/// Runs `work` for at most `limit` (no limit when `None`), with the timer registered the first time `work` cannot go
/// on, like [`within`]: the limit is an idle time, so it starts anew for every call.
pub(in crate::http::proxy) async fn within_for<F: Future>(
    limit: Option<Duration>,
    work: F,
) -> Result<F::Output, Expired> {
    let Some(limit) = limit else {
        return Ok(work.await);
    };
    let mut work = pin!(work);
    let mut timer: Option<Pin<Box<Sleep>>> = None;
    poll_fn(|cx| {
        if let Poll::Ready(output) = work.as_mut().poll(cx) {
            return Poll::Ready(Ok(output));
        }
        let timer = timer.get_or_insert_with(|| Box::pin(sleep(limit)));
        timer.as_mut().poll(cx).map(|()| Err(Expired))
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn work_that_finishes_is_returned() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(within(deadline, async { 7 }).await.unwrap(), 7);
        let slow = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            8
        };
        assert_eq!(within(deadline, slow).await.unwrap(), 8);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_limit_starts_anew_for_each_call_and_does_not_bound_work_that_does_not_wait() {
        let limit = Some(Duration::from_millis(50));
        for _ in 0..3 {
            let wait = tokio::time::sleep(Duration::from_millis(30));
            assert!(within_for(limit, wait).await.is_ok());
        }
        assert_eq!(within_for(limit, async { 1 }).await.unwrap(), 1);
        let stuck = tokio::time::sleep(Duration::from_mins(1));
        assert!(within_for(limit, stuck).await.is_err());
        let unbounded = tokio::time::sleep(Duration::from_secs(5));
        assert!(within_for(None, unbounded).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn work_that_waits_past_the_deadline_expires() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let stuck = tokio::time::sleep(Duration::from_mins(1));
        assert!(within(deadline, stuck).await.is_err());
        assert!(Instant::now() >= deadline);
    }
}
