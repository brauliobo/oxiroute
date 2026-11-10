use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// Returns `Pending` once after waking its own task: the task goes to the back of the ready queue and runs again.
pub struct Reschedule(bool);

pub fn reschedule() -> Reschedule {
    Reschedule(false)
}

impl Future for Reschedule {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
