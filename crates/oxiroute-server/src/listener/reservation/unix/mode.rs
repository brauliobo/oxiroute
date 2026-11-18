use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use log::warn;

use super::UnixSocketIdentity;
use crate::listener::reservation::ListenerReservation;

pub(crate) struct UnixModeChange {
    socket: ListenerReservation,
    previous: u16,
    settled: AtomicBool,
}

impl UnixModeChange {
    pub(crate) fn apply(
        socket: &ListenerReservation,
        mode: Option<u16>,
    ) -> io::Result<Option<Arc<Self>>> {
        let unix = socket.unix_socket()?;
        let (previous, target) = (unix.mode(), unix.target_mode(mode));
        if previous == target {
            return Ok(None);
        }
        unix.set_mode(target)?;
        Ok(Some(Arc::new(Self {
            socket: socket.clone(),
            previous,
            settled: AtomicBool::new(false),
        })))
    }

    pub(crate) fn settle(&self) {
        self.settled.store(true, Ordering::Release);
    }
}

impl Drop for UnixModeChange {
    fn drop(&mut self) {
        if self.settled.load(Ordering::Acquire) {
            return;
        }
        if let Some(unix) = &self.socket.inner.unix_socket
            && let Err(error) = unix.set_mode(self.previous)
        {
            warn!(
                "listener Unix socket mode could not be restored to {:o}: {error}",
                self.previous
            );
        }
    }
}

impl ListenerReservation {
    pub(crate) fn unix_socket(&self) -> io::Result<&UnixSocketIdentity> {
        self.inner.unix_socket.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a Unix socket adopted from the supervising master keeps the mode the master gave it",
            )
        })
    }
}
