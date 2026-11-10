//! A deadline that costs a timer only when the work it bounds has to wait.

use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::Poll,
};

use tokio::time::{Instant, Sleep, sleep_until};

/// The work did not finish by its deadline.
#[derive(Debug)]
pub(super) struct Expired;

/// Runs `work` and gives up at `deadline`. The timer is registered the first time `work` cannot go on, so work that
/// completes on its first poll (a response that fits the socket buffers) never touches the timer wheel.
pub(super) async fn within<F: Future>(deadline: Instant, work: F) -> Result<F::Output, Expired> {
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
    async fn work_that_waits_past_the_deadline_expires() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let stuck = tokio::time::sleep(Duration::from_secs(60));
        assert!(within(deadline, stuck).await.is_err());
        assert!(Instant::now() >= deadline);
    }
}
