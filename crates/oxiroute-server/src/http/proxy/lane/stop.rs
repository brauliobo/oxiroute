//! The service's stop signal as one connection sees it.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::sync::watch;

/// A connection's view of the stop signal. Waiting on a `watch` costs a lock of its waiter list for every poll, so the
/// wait is polled once, which registers the connection's waker for as long as the wait lives, and again only when the
/// signal changed (one atomic load to find out).
pub(super) struct Stop<'a, F> {
    /// A receiver of its own: the wait borrows the one it was made from.
    probe: watch::Receiver<bool>,
    /// The service was stopping before this connection looked.
    already: bool,
    wait: Pin<&'a mut F>,
    armed: bool,
}

impl<'a, F: Future<Output = ()>> Stop<'a, F> {
    /// `wait` resolves when the signal `shutdown` carries turns true.
    pub(super) fn new(shutdown: &watch::Receiver<bool>, wait: Pin<&'a mut F>) -> Self {
        Self {
            probe: shutdown.clone(),
            already: *shutdown.borrow(),
            wait,
            armed: false,
        }
    }

    /// Whether the service is stopping.
    pub(super) fn stopping(&self) -> bool {
        self.already || self.probe.has_changed().unwrap_or(true)
    }

    /// Ready once the service is stopping; the connection ends then and does not poll again.
    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.armed && !self.stopping() {
            return Poll::Pending;
        }
        self.armed = true;
        self.wait.as_mut().poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::{future::poll_fn, pin::pin};

    use super::*;

    async fn until_true(mut watch: watch::Receiver<bool>) {
        let _ = watch.wait_for(|stopping| *stopping).await;
    }

    #[tokio::test]
    async fn a_stop_after_the_first_poll_wakes_the_connection() {
        let (sender, receiver) = watch::channel(false);
        let seen = receiver.clone();
        let waits = tokio::spawn(async move {
            let mut waiting = pin!(until_true(seen.clone()));
            let mut stop = Stop::new(&seen, waiting.as_mut());
            assert!(!stop.stopping());
            poll_fn(|cx| stop.poll(cx)).await;
            stop.stopping()
        });
        tokio::task::yield_now().await;
        assert!(!waits.is_finished(), "nothing was stopped yet");
        sender.send(true).expect("a receiver");
        assert!(waits.await.expect("task"));
    }

    #[tokio::test]
    async fn a_service_that_was_stopping_is_seen_at_once() {
        let (sender, receiver) = watch::channel(false);
        sender.send(true).expect("a receiver");
        let mut waiting = pin!(until_true(receiver.clone()));
        let mut stop = Stop::new(&receiver, waiting.as_mut());
        assert!(stop.stopping());
        poll_fn(|cx| stop.poll(cx)).await;
    }
}
