use std::{io, os::fd::OwnedFd, sync::Arc};

use rustix::event::{EventfdFlags, eventfd};

#[derive(Clone)]
pub struct Waker(Arc<OwnedFd>);

impl Waker {
    pub(crate) fn new() -> io::Result<Self> {
        let fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
        Ok(Self(Arc::new(fd)))
    }

    pub fn wake(&self) {
        let _ = rustix::io::write(&*self.0, &1_u64.to_ne_bytes());
    }

    pub(crate) fn fd(&self) -> &OwnedFd {
        &self.0
    }

    pub(crate) fn settle(&self) {
        let mut counter = [0_u8; 8];
        let _ = rustix::io::read(&*self.0, &mut counter);
    }
}
