use std::{io, net::SocketAddr};

use rustix::{io::Errno, net::Shutdown};

use super::Ring;
use crate::{
    event::{Event, Sent},
    raw::{Completion, Memory},
    slot::{Connect, Handle, Kind, Writer},
};

impl Ring {
    /// Stages `head` then `body` for sending. The ring owns the staged copy, so the caller's buffers are free
    /// again as soon as this returns; one send is in flight per socket.
    pub fn send(&mut self, handle: Handle, head: &[u8], body: &[u8]) -> Sent {
        let Some(slot) = self.slot(handle).filter(|slot| !slot.closing) else {
            return Sent::Failed;
        };
        match slot.writer {
            Writer::Broken => return Sent::Failed,
            Writer::Flying { .. } => {
                slot.want_write = true;
                return Sent::Busy;
            }
            Writer::Idle => {}
        }
        let total = head.len() + body.len();
        let staged = total.min(self.config.send_limit);
        if staged == 0 {
            return Sent::Queued(0);
        }
        let mut staging = self.take_staging();
        let from_head = staged.min(head.len());
        staging.extend_from_slice(&head[..from_head]);
        staging.extend_from_slice(&body[..staged - from_head]);
        if self.launch_send(handle, staging, 0).is_err() {
            return self.send_broke(handle);
        }
        if let Some(slot) = self.slot(handle) {
            slot.want_write = staged < total;
        }
        Sent::Queued(staged)
    }

    fn launch_send(&mut self, handle: Handle, staging: Vec<u8>, from: usize) -> io::Result<()> {
        let meta = Self::meta(Kind::Send, 0, handle);
        let key = self
            .with_fd(handle, |queue, fd| queue.send(fd, staging, from, meta))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))??;
        if let Some(slot) = self.slot_any(handle) {
            slot.writer = Writer::Flying { key, from };
            slot.inflight += 1;
        }
        Ok(())
    }

    fn send_broke(&mut self, handle: Handle) -> Sent {
        if let Some(slot) = self.slot(handle) {
            slot.writer = Writer::Broken;
        }
        Sent::Failed
    }

    pub(crate) fn sent(&mut self, handle: Handle, done: Completion) {
        let Memory::Bytes(staging) = done.memory else {
            return;
        };
        let Some(slot) = self.slot_any(handle) else {
            return;
        };
        let from = match slot.writer {
            Writer::Flying { from, .. } => from,
            _ => 0,
        };
        slot.writer = Writer::Idle;
        let written = usize::try_from(done.result)
            .ok()
            .filter(|written| *written > 0);
        match written {
            Some(written) if from + written < staging.len() => {
                if self.launch_send(handle, staging, from + written).is_err() {
                    self.send_failed(handle);
                }
            }
            Some(_) => {
                self.give_staging(staging);
                self.send_finished(handle);
            }
            None => {
                self.give_staging(staging);
                self.send_failed(handle);
            }
        }
    }

    fn send_finished(&mut self, handle: Handle) {
        let Some(slot) = self.slot_any(handle) else {
            return;
        };
        let wanted = std::mem::take(&mut slot.want_write);
        if std::mem::take(&mut slot.shutdown_after_send) {
            self.shutdown_now(handle);
        }
        if wanted {
            self.emit(handle, |tag| Event::Writable { tag });
        }
    }

    fn send_failed(&mut self, handle: Handle) {
        if let Some(slot) = self.slot_any(handle) {
            slot.writer = Writer::Broken;
        }
        self.emit(handle, |tag| Event::Failed { tag });
    }

    /// Ends the sending direction once the send in flight, if any, has been written.
    pub fn shutdown_write(&mut self, handle: Handle) {
        let Some(slot) = self.slot(handle) else {
            return;
        };
        if matches!(slot.writer, Writer::Flying { .. }) {
            slot.shutdown_after_send = true;
        } else {
            self.shutdown_now(handle);
        }
    }

    fn shutdown_now(&mut self, handle: Handle) {
        if let Some(fd) = self.slot_any(handle).and_then(|slot| slot.fd.as_ref()) {
            let _ = rustix::net::shutdown(fd, Shutdown::Write);
        }
    }

    /// Connects an unconnected socket; `Event::Connected` or `Event::Failed` follows.
    pub fn connect(&mut self, handle: Handle, peer: SocketAddr) {
        let meta = Self::meta(Kind::Connect, 0, handle);
        let launched = self.with_fd(handle, |queue, fd| queue.connect(fd, peer, meta));
        match (launched, self.slot(handle)) {
            (Some(Ok(key)), Some(slot)) => {
                slot.connect = Connect::Flying(key);
                slot.inflight += 1;
            }
            (_, Some(slot)) => {
                slot.connect = Connect::Done(false);
                self.emit(handle, |tag| Event::Failed { tag });
            }
            _ => {}
        }
    }

    /// Whether a finished connect succeeded; `None` while it is in flight or was never started.
    pub fn connected(&mut self, handle: Handle) -> Option<bool> {
        match self.slot(handle)?.connect {
            Connect::Done(ok) => Some(ok),
            Connect::Idle | Connect::Flying(_) => None,
        }
    }

    pub(crate) fn connected_to(&mut self, handle: Handle, result: i32) {
        let ok = result == 0;
        let Some(slot) = self.slot_any(handle) else {
            return;
        };
        slot.connect = Connect::Done(ok);
        let canceled = Self::errno(result) == Some(Errno::CANCELED);
        if ok {
            self.emit(handle, |tag| Event::Connected { tag });
            self.arm_recv(handle);
        } else if !canceled {
            self.emit(handle, |tag| Event::Failed { tag });
        }
    }
}
