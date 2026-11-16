use rustix::io::Errno;

use super::Ring;
use crate::{
    event::{Event, Read},
    raw::Completion,
    slot::{Handle, Kind, Reader},
};

impl Ring {
    /// Starts watching a connected socket: it is writable and a receive is armed.
    pub fn start(&mut self, handle: Handle) {
        self.emit(handle, |tag| Event::Writable { tag });
        self.arm_recv(handle);
    }

    pub(crate) fn arm_recv(&mut self, handle: Handle) {
        let idle = self
            .slot(handle)
            .is_some_and(|slot| !slot.closing && matches!(slot.reader, Reader::Idle));
        if !idle {
            return;
        }
        let meta = Self::meta(Kind::Recv, 0, handle);
        let armed = self.with_fd(handle, |queue, fd| queue.recv(fd, meta));
        self.reader_armed(handle, armed.and_then(Result::ok));
    }

    fn reader_armed(&mut self, handle: Handle, armed: Option<u64>) {
        let Some(slot) = self.slot(handle) else {
            return;
        };
        if let Some(key) = armed {
            slot.reader = Reader::Armed(key);
            slot.inflight += 1;
        } else {
            slot.reader = Reader::Broken;
            self.emit(handle, |tag| Event::Failed { tag });
        }
    }

    /// Copies received bytes into `dst`. Returns `Block` while nothing was received, which also covers a socket
    /// whose receive buffer pool ran dry: the ring arms the receive again as soon as a buffer returns.
    pub fn read(&mut self, handle: Handle, dst: &mut [u8]) -> Read {
        let Some(slot) = self.slot(handle) else {
            return Read::Failed;
        };
        match slot.reader {
            Reader::Eof => Read::Eof,
            Reader::Broken => Read::Failed,
            Reader::Parked { buffer, taken, len } if !dst.is_empty() => {
                let room = dst.len().min(len - taken);
                let copied = self
                    .queue
                    .buffers()
                    .copy_out(buffer, taken, &mut dst[..room]);
                self.consumed(handle, buffer, taken + copied, len);
                Read::Data(copied)
            }
            _ => Read::Block,
        }
    }

    fn consumed(&mut self, handle: Handle, buffer: u16, taken: usize, len: usize) {
        let Some(slot) = self.slot(handle) else {
            return;
        };
        if taken < len {
            slot.reader = Reader::Parked { buffer, taken, len };
            return;
        }
        slot.reader = Reader::Idle;
        self.queue.buffers().give(buffer);
        self.arm_recv(handle);
        self.rearm_starved();
    }

    pub(crate) fn rearm_starved(&mut self) {
        while let Some(handle) = self.starved.pop_front() {
            let Some(slot) = self.slot(handle) else {
                continue;
            };
            if matches!(slot.reader, Reader::Starved) {
                slot.reader = Reader::Idle;
                return self.arm_recv(handle);
            }
        }
    }

    pub(crate) fn received(&mut self, handle: Handle, done: &Completion) {
        let closing = self.slot_any(handle).is_none_or(|slot| slot.closing);
        let received = usize::try_from(done.result).ok().filter(|len| *len > 0);
        let data = done.buffer.zip(received).filter(|_| !closing);
        if let Some(buffer) = done.buffer.filter(|_| data.is_none()) {
            self.queue.buffers().give(buffer);
            self.rearm_starved();
        }
        if closing {
            if let Some(slot) = self.slot_any(handle) {
                slot.reader = Reader::Idle;
            }
            return;
        }
        let Some(slot) = self.slot_any(handle) else {
            return;
        };
        match data {
            Some((buffer, len)) => {
                slot.reader = Reader::Parked {
                    buffer,
                    taken: 0,
                    len,
                };
                self.emit(handle, |tag| Event::Readable { tag });
            }
            None if done.result == 0 => {
                slot.reader = Reader::Eof;
                self.emit(handle, |tag| Event::Eof { tag });
            }
            None => {
                slot.reader = Reader::Idle;
                self.receive_failed(handle, Self::errno(done.result));
            }
        }
    }

    fn receive_failed(&mut self, handle: Handle, error: Option<Errno>) {
        match error {
            Some(Errno::NOBUFS) => {
                if let Some(slot) = self.slot_any(handle) {
                    slot.reader = Reader::Starved;
                }
                self.starved.push_back(handle);
            }
            Some(Errno::INTR | Errno::AGAIN | Errno::CANCELED) => self.arm_recv(handle),
            _ => {
                if let Some(slot) = self.slot_any(handle) {
                    slot.reader = Reader::Broken;
                }
                self.emit(handle, |tag| Event::Failed { tag });
            }
        }
    }

    /// Reports once when the descriptor becomes readable, for descriptors that are not sockets (an eventfd).
    pub fn notify(&mut self, handle: Handle) {
        let waiting = self
            .slot(handle)
            .is_none_or(|slot| slot.closing || slot.poll.is_some());
        if waiting {
            return;
        }
        let meta = Self::meta(Kind::Poll, 0, handle);
        let armed = self.with_fd(handle, |queue, fd| queue.poll_in(fd, meta));
        if let (Some(Ok(key)), Some(slot)) = (armed, self.slot(handle)) {
            slot.poll = Some(key);
            slot.inflight += 1;
        }
    }

    pub(crate) fn polled(&mut self, handle: Handle) {
        if let Some(slot) = self.slot_any(handle) {
            slot.poll = None;
        }
        self.emit(handle, |tag| Event::Notified { tag });
    }
}
