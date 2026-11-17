use rustix::io::Errno;

use super::Ring;
use crate::{
    event::{Event, Read},
    raw::{Completion, POOLS},
    slot::{Chunk, Handle, Kind, Lease, Reader},
};

impl Ring {
    /// Starts watching a connected socket: it is writable and a receive is armed.
    pub fn start(&mut self, handle: Handle) {
        self.emit(handle, |tag| Event::Writable { tag });
        self.arm_recv(handle);
    }

    pub(crate) fn arm_recv(&mut self, handle: Handle) {
        let Some(pool) = self
            .slot(handle)
            .filter(|slot| !slot.closing && matches!(slot.reader, Reader::Idle))
            .map(|slot| u8::from(slot.streaming))
        else {
            return;
        };
        let meta = Self::meta(Kind::Recv, pool, handle);
        let armed = self.with_fd(handle, |queue, fd| queue.recv(fd, pool, meta));
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
        match std::mem::replace(&mut slot.reader, Reader::Idle) {
            Reader::Parked { chunk, taken, len } if !dst.is_empty() => {
                let room = dst.len().min(len - taken);
                let copied = self.copy_out(&chunk, taken, &mut dst[..room]);
                self.consumed(handle, (chunk, taken + copied, len));
                Read::Data(copied)
            }
            reader => {
                slot.reader = reader;
                match slot.reader {
                    Reader::Eof => Read::Eof,
                    Reader::Broken => Read::Failed,
                    _ => Read::Block,
                }
            }
        }
    }

    pub(crate) fn copy_out(&mut self, chunk: &Chunk, from: usize, dst: &mut [u8]) -> usize {
        match chunk {
            Chunk::Ring(lease) => self
                .queue
                .held(lease.pool)
                .copy_out(lease.buffer, from, dst),
            Chunk::Heap(bytes) => {
                dst.copy_from_slice(&bytes[from..from + dst.len()]);
                dst.len()
            }
        }
    }

    pub(crate) fn consumed(&mut self, handle: Handle, (chunk, taken, len): (Chunk, usize, usize)) {
        if taken < len {
            if let Some(slot) = self.slot(handle) {
                slot.reader = Reader::Parked { chunk, taken, len };
            }
            return;
        }
        self.discard(chunk);
        self.arm_recv(handle);
    }

    pub(crate) fn discard(&mut self, chunk: Chunk) {
        match chunk {
            Chunk::Ring(lease) => self.give_buffer(lease),
            Chunk::Heap(bytes) => self.give_spill(bytes),
        }
    }

    /// Moves a chunk out of the provided buffers when few are left, so bytes nobody reads for a while can never pin
    /// the pool and stop every other socket from receiving.
    fn parked(&mut self, lease: Lease, len: usize) -> Chunk {
        let pool = &self.pools[usize::from(lease.pool)];
        if pool.available > pool.count / 8 {
            return Chunk::Ring(lease);
        }
        let mut bytes = self.spills.pop().unwrap_or_default();
        bytes.resize(len, 0);
        self.queue
            .held(lease.pool)
            .copy_out(lease.buffer, 0, &mut bytes);
        self.give_buffer(lease);
        Chunk::Heap(bytes)
    }

    fn give_spill(&mut self, mut bytes: Vec<u8>) {
        bytes.clear();
        if self.spills.len() < 256 {
            self.spills.push(bytes);
        }
    }

    pub(crate) fn give_buffer(&mut self, lease: Lease) {
        self.queue.buffers(lease.pool).give(lease.buffer);
        self.pools[usize::from(lease.pool)].available += 1;
        self.rearm_starved();
    }

    pub(crate) fn rearm_starved(&mut self) {
        for pool in 0..POOLS {
            self.rearm_pool(pool);
        }
    }

    /// Arms the receives of the sockets starved of a pool, as many as there are buffers. A starved socket had data
    /// when the pool was empty (receives wait for readiness before they select a buffer), so each one takes a buffer.
    fn rearm_pool(&mut self, pool: usize) {
        let mut budget = self.pools[pool].available;
        while budget > 0 {
            let Some(handle) = self.pools[pool].starved.pop_front() else {
                return;
            };
            let Some(slot) = self.slot(handle) else {
                continue;
            };
            if matches!(slot.reader, Reader::Starved) {
                slot.reader = Reader::Idle;
                self.arm_recv(handle);
                budget -= 1;
            }
        }
    }

    pub(crate) fn received(&mut self, handle: Handle, pool: u8, done: &Completion) {
        let closing = self.slot_any(handle).is_none_or(|slot| slot.closing);
        let taken = done
            .buffer
            .filter(|_| done.result >= 0)
            .map(|buffer| Lease { pool, buffer });
        if taken.is_some() {
            let available = &mut self.pools[usize::from(pool)].available;
            *available = available.saturating_sub(1);
        }
        let received = usize::try_from(done.result).ok().filter(|len| *len > 0);
        if closing {
            self.received_after_close(handle, taken.map(|buffer| (buffer, received.unwrap_or(0))));
            return;
        }
        let data = taken.zip(received);
        if let Some(lease) = taken.filter(|_| data.is_none()) {
            self.give_buffer(lease);
        }
        match data {
            Some((lease, len)) => {
                let chunk = self.parked(lease, len);
                let streaming = len >= self.config.buffer_size as usize;
                if let Some(slot) = self.slot_any(handle) {
                    slot.streaming = streaming;
                    slot.reader = Reader::Parked {
                        chunk,
                        taken: 0,
                        len,
                    };
                }
                self.emit(handle, |tag| Event::Readable { tag });
            }
            None if done.result == 0 => {
                if let Some(slot) = self.slot_any(handle) {
                    slot.reader = Reader::Eof;
                }
                self.emit(handle, |tag| Event::Eof { tag });
            }
            None => {
                if let Some(slot) = self.slot_any(handle) {
                    slot.reader = Reader::Idle;
                }
                self.receive_failed(handle, pool, Self::errno(done.result));
            }
        }
    }

    /// A receive that ended after the socket was given up. Its bytes are gone for a closed socket and belong to the
    /// owner of a released one, whose descriptor will never deliver them again.
    fn received_after_close(&mut self, handle: Handle, taken: Option<(Lease, usize)>) {
        let bytes = taken.map(|(lease, len)| {
            let mut bytes = vec![0; len];
            self.queue
                .held(lease.pool)
                .copy_out(lease.buffer, 0, &mut bytes);
            self.give_buffer(lease);
            bytes
        });
        if let Some(slot) = self.slot_any(handle) {
            slot.reader = Reader::Idle;
            if slot.releasing {
                slot.unread.extend(bytes.into_iter().flatten());
            }
        }
    }

    /// Takes the bytes of the socket that were received and not read yet.
    pub(crate) fn take_parked(&mut self, handle: Handle) -> Vec<u8> {
        let Some(slot) = self.slot(handle) else {
            return Vec::new();
        };
        match std::mem::replace(&mut slot.reader, Reader::Idle) {
            Reader::Parked { chunk, taken, len } => {
                let mut bytes = vec![0; len - taken];
                self.copy_out(&chunk, taken, &mut bytes);
                self.discard(chunk);
                bytes
            }
            reader => {
                slot.reader = reader;
                Vec::new()
            }
        }
    }

    fn receive_failed(&mut self, handle: Handle, pool: u8, error: Option<Errno>) {
        let pool = usize::from(pool);
        match error {
            Some(Errno::NOBUFS) if self.pools[pool].available > 0 => self.arm_recv(handle),
            Some(Errno::NOBUFS) => {
                if let Some(slot) = self.slot_any(handle) {
                    slot.reader = Reader::Starved;
                }
                self.pools[pool].starved.push_back(handle);
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
        let Some(slot) = self.slot_any(handle) else {
            return;
        };
        slot.poll = None;
        if slot.acceptor.is_some() {
            self.connection_waits(handle);
        } else {
            self.emit(handle, |tag| Event::Notified { tag });
        }
    }
}
