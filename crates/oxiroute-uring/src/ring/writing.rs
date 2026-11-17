use std::{io, io::IoSlice, net::SocketAddr};

use rustix::{
    io::Errno,
    net::{SendAncillaryBuffer, SendFlags, Shutdown},
};

use super::Ring;
use crate::{
    event::{Event, Sent},
    raw::{Completion, Memory},
    slot::{Connect, Handle, Kind, Reader, Writer},
};

impl Ring {
    /// Sends `head` then `body` with the next submission, in one batch with every other operation of the thread.
    /// The ring owns a copy of up to `Config::send_limit` bytes, which it completes on its own, so the caller's
    /// buffers are free again as soon as this returns; one staged send is in flight per socket.
    pub fn send(&mut self, handle: Handle, head: &[u8], body: &[u8]) -> Sent {
        self.send_with(handle, (head, body), false)
    }

    /// Like [`Ring::send`], but the socket takes what it can at once: for a socket that is closed right after,
    /// so its last bytes and its end reach the peer in order and without waiting for a submission.
    pub fn send_now(&mut self, handle: Handle, head: &[u8], body: &[u8]) -> Sent {
        self.send_with(handle, (head, body), true)
    }

    pub(crate) fn send_with(
        &mut self,
        handle: Handle,
        (head, body): (&[u8], &[u8]),
        now: bool,
    ) -> Sent {
        if let Err(refused) = self.writable(handle) {
            return refused;
        }
        let total = head.len() + body.len();
        if total == 0 {
            return Sent::Queued(0);
        }
        let written = if now {
            self.write_now(handle, head, body)
        } else {
            Some(0)
        };
        let Some(written) = written else {
            return self.send_broke(handle);
        };
        if written == total {
            return Sent::Queued(total);
        }
        let skipped = written.min(head.len());
        let rest = (&head[skipped..], &body[written - skipped..]);
        match self.stage(handle, rest) {
            Ok(staged) => Sent::Queued(written + staged),
            Err(_) => self.send_broke(handle),
        }
    }

    pub(crate) fn writable(&mut self, handle: Handle) -> Result<(), Sent> {
        let Some(slot) = self.slot(handle).filter(|slot| !slot.closing) else {
            return Err(Sent::Failed);
        };
        match slot.writer {
            Writer::Broken => Err(Sent::Failed),
            Writer::Flying { .. } => {
                slot.want_write = true;
                Err(Sent::Busy)
            }
            Writer::Idle => Ok(()),
        }
    }

    pub(crate) fn write_now(&self, handle: Handle, head: &[u8], body: &[u8]) -> Option<usize> {
        let fd = self.slot_ref(handle)?.fd.as_ref()?;
        let slices = [IoSlice::new(head), IoSlice::new(body)];
        loop {
            let sent = rustix::net::sendmsg(
                fd,
                &slices,
                &mut SendAncillaryBuffer::default(),
                SendFlags::NOSIGNAL,
            );
            return match sent {
                Ok(written) => Some(written),
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => Some(0),
                Err(_) => None,
            };
        }
    }

    fn stage(&mut self, handle: Handle, (head, body): (&[u8], &[u8])) -> io::Result<usize> {
        let total = head.len() + body.len();
        let staged = total.min(self.config.send_limit);
        let mut staging = self.take_staging();
        let from_head = staged.min(head.len());
        staging.extend_from_slice(&head[..from_head]);
        staging.extend_from_slice(&body[..staged - from_head]);
        self.launch_send(handle, staging)?;
        if let Some(slot) = self.slot(handle) {
            slot.want_write = staged < total;
        }
        Ok(staged)
    }

    fn launch_send(&mut self, handle: Handle, staging: Vec<u8>) -> io::Result<()> {
        let meta = Self::meta(Kind::Send, 0, handle);
        let key = self
            .with_fd(handle, |queue, fd| queue.send(fd, staging, meta))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))??;
        self.flying(handle, key);
        Ok(())
    }

    pub(crate) fn flying(&mut self, handle: Handle, key: u64) {
        if let Some(slot) = self.slot_any(handle) {
            slot.writer = Writer::Flying { key };
            slot.inflight += 1;
        }
    }

    pub(crate) fn send_broke(&mut self, handle: Handle) -> Sent {
        if let Some(slot) = self.slot(handle) {
            slot.writer = Writer::Broken;
        }
        Sent::Failed
    }

    pub(crate) fn sent(&mut self, handle: Handle, done: Completion) {
        let Some(slot) = self.slot_any(handle) else {
            return;
        };
        slot.writer = Writer::Idle;
        let payload = done.memory.payload();
        let complete = usize::try_from(done.result).is_ok_and(|written| written == payload);
        self.reclaim(done.memory);
        if complete {
            self.send_finished(handle);
        } else {
            self.send_failed(handle);
        }
    }

    fn reclaim(&mut self, memory: Memory) {
        match memory {
            Memory::Bytes(staging) => self.give_staging(staging),
            Memory::Lent(lent) => {
                self.give_buffer(lent.lease);
                self.give_staging(lent.head);
            }
            Memory::None | Memory::Address(_) => {}
        }
    }

    fn send_finished(&mut self, handle: Handle) {
        let Some(slot) = self.slot_any(handle) else {
            return;
        };
        let wanted = std::mem::take(&mut slot.want_write);
        let closing = slot.closing;
        if std::mem::take(&mut slot.shutdown_after_send) {
            self.shutdown_now(handle, Shutdown::Write);
        }
        if closing {
            self.hang_up(handle, Shutdown::Both);
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
        self.hang_up(handle, Shutdown::Read);
    }

    /// Ends the sending direction once the send in flight, if any, has been written.
    pub fn shutdown_write(&mut self, handle: Handle) {
        let Some(slot) = self.slot(handle) else {
            return;
        };
        if matches!(slot.writer, Writer::Flying { .. }) {
            slot.shutdown_after_send = true;
        } else {
            self.shutdown_now(handle, Shutdown::Write);
        }
    }

    fn shutdown_now(&mut self, handle: Handle, how: Shutdown) {
        if let Some(fd) = self.slot_any(handle).and_then(|slot| slot.fd.as_ref()) {
            let _ = rustix::net::shutdown(fd, how);
        }
    }

    pub(crate) fn hang_up(&mut self, handle: Handle, how: Shutdown) {
        let armed = self.slot_any(handle).is_some_and(|slot| {
            slot.closing && !slot.releasing && matches!(slot.reader, Reader::Armed(_))
        });
        if armed {
            self.shutdown_now(handle, how);
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
