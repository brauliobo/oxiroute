use std::os::fd::AsFd as _;

#[allow(clippy::wildcard_imports)]
use super::*;

/// Lets the signal and file-watcher threads end the master loop's wait.
pub(super) struct Waker(OwnedFd);

impl Waker {
    pub(super) fn new() -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self(eventfd(
            0,
            EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK,
        )?)))
    }

    pub(super) fn wake(&self) {
        // The only failure is a counter that is already saturated, which means a wake is pending.
        let _ = rustix::io::write(&self.0, &1_u64.to_ne_bytes());
    }

    pub(super) fn descriptor(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    pub(super) fn drain(&self) {
        // An empty counter reports `EAGAIN`: nothing was pending.
        let _ = rustix::io::read(&self.0, &mut [0_u8; 8]);
    }
}
