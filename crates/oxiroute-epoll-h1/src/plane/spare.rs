use std::{io, os::fd::OwnedFd};

use super::sys;

pub(crate) struct Spare(Option<OwnedFd>);

impl Spare {
    pub(crate) fn reserve() -> io::Result<Self> {
        sys::spare_descriptor().map(Some).map(Self)
    }

    pub(crate) fn shed(&mut self, listener: &OwnedFd) {
        self.0 = None;
        drop(sys::accept(listener));
        self.0 = sys::spare_descriptor().ok();
    }
}
