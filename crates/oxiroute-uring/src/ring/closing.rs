use std::{
    io,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

use super::Ring;
use crate::slot::{Connect, Handle, Reader, Writer};

impl Ring {
    /// Gives the socket up. Receives, connects, polls and accepts are cancelled; a send already accepted is
    /// allowed to finish for at most `Config::flush`; the descriptor closes once nothing uses it. Late
    /// completions of the socket are absorbed here and never reach the caller.
    pub fn close(&mut self, handle: Handle) {
        if !self.retire(handle) {
            return;
        }
        let flushing = self
            .slot(handle)
            .is_some_and(|slot| matches!(slot.writer, Writer::Flying { .. }));
        if flushing {
            self.zombies
                .push_back((Instant::now() + self.config.flush, handle));
        }
        self.finalize(handle);
    }

    fn retire(&mut self, handle: Handle) -> bool {
        let Some(slot) = self.slot(handle).filter(|slot| !slot.closing) else {
            return false;
        };
        slot.closing = true;
        let mut targets = Vec::new();
        targets.extend(slot.poll);
        if let Reader::Armed(key) = slot.reader {
            targets.push(key);
        }
        if let Connect::Flying(key) = slot.connect {
            targets.push(key);
        }
        let parked = match std::mem::replace(&mut slot.reader, Reader::Idle) {
            Reader::Parked { chunk, .. } => Some(chunk),
            reader => {
                slot.reader = reader;
                None
            }
        };
        for key in targets {
            let _ = self.queue.cancel(key);
        }
        self.cancel_accepts(handle);
        if let Some(chunk) = parked {
            self.discard(chunk);
        }
        true
    }

    pub(crate) fn finalize(&mut self, handle: Handle) {
        let Some(slot) = self.slots.get_mut(handle.index as usize) else {
            return;
        };
        if !(slot.live
            && slot.generation == handle.generation
            && slot.closing
            && !slot.releasing
            && slot.idle())
        {
            return;
        }
        let fd = slot.fd.take();
        *slot = crate::slot::Slot::vacant_after(slot.generation);
        self.free.push(handle.index);
        drop(fd);
    }

    /// Takes the descriptor back out of the ring, for a connection that leaves the data plane. Operations in
    /// flight are cancelled and waited for, so nothing of the ring touches the descriptor afterwards. A send
    /// that cannot finish within `Config::flush` fails the release and closes the socket.
    pub fn release(&mut self, handle: Handle) -> io::Result<OwnedFd> {
        if !self.retire(handle) {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        if let Some(slot) = self.slot(handle) {
            slot.releasing = true;
        }
        let deadline = Instant::now() + self.config.flush;
        while self.busy(handle) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                if let Some(slot) = self.slot(handle) {
                    slot.releasing = false;
                }
                self.abandon(handle);
                self.finalize(handle);
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            self.queue.wait(Some(left))?;
            self.settle();
        }
        let slot = self
            .slots
            .get_mut(handle.index as usize)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let fd = slot
            .fd
            .take()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        *slot = crate::slot::Slot::vacant_after(slot.generation);
        self.free.push(handle.index);
        Ok(fd)
    }

    fn busy(&self, handle: Handle) -> bool {
        self.slots
            .get(handle.index as usize)
            .is_some_and(|slot| slot.generation == handle.generation && slot.live && !slot.idle())
    }

    fn abandon(&mut self, handle: Handle) {
        let key = self.slot(handle).and_then(|slot| match slot.writer {
            Writer::Flying { key, .. } => Some(key),
            _ => None,
        });
        if let Some(key) = key {
            let _ = self.queue.cancel(key);
        }
    }

    pub(crate) fn bounded(&self, timeout: Option<Duration>) -> Option<Duration> {
        let next = self
            .zombies
            .front()
            .map(|(deadline, _)| deadline.saturating_duration_since(Instant::now()));
        match (timeout, next) {
            (Some(timeout), Some(next)) => Some(timeout.min(next)),
            (timeout, next) => timeout.or(next),
        }
    }

    pub(crate) fn expire_zombies(&mut self) {
        let now = Instant::now();
        while let Some((deadline, handle)) = self.zombies.front().copied() {
            if deadline > now {
                return;
            }
            self.zombies.pop_front();
            self.abandon(handle);
        }
    }
}
