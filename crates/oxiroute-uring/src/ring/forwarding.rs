use std::ops::Range;

use super::Ring;
use crate::{
    event::{Forwarded, Peek, Sent},
    slot::{Chunk, Handle, Kind, Lease, Reader},
};

impl Ring {
    /// Shows the bytes received from a socket that nobody took yet, without copying them out.
    pub fn peek<R>(&self, handle: Handle, view: impl FnOnce(&[u8]) -> R) -> Peek<R> {
        let Some(slot) = self.slot_ref(handle) else {
            return Peek::Failed;
        };
        match &slot.reader {
            Reader::Parked { chunk, taken, len } => {
                Peek::Held(view(self.parked_bytes(chunk, *taken, *len)))
            }
            Reader::Eof => Peek::Eof,
            Reader::Broken => Peek::Failed,
            _ => Peek::Block,
        }
    }

    fn parked_bytes<'a>(&'a self, chunk: &'a Chunk, taken: usize, len: usize) -> &'a [u8] {
        match chunk {
            Chunk::Ring(lease) => {
                self.queue
                    .held(lease.pool)
                    .view(lease.buffer, taken, len - taken)
            }
            Chunk::Heap(bytes) => &bytes[taken..len],
        }
    }

    /// Sends `head` and then the first `count` unread bytes of `from` on `to`. When those are all the bytes the
    /// receive buffer holds, the buffer itself is what the send reads and it goes back to the pool when the send
    /// ends: the bytes are never copied in user space. The receive of `from` is armed again at once.
    pub fn forward(
        &mut self,
        (from, to): (Handle, Handle),
        head: &[u8],
        count: usize,
        now: bool,
    ) -> Forwarded {
        if let Err(sent) = self.writable(to) {
            return self.forwarded(from, sent);
        }
        let Some((chunk, taken, len)) = self.unpark(from) else {
            return Forwarded {
                sent: Sent::Failed,
                left: 0,
            };
        };
        let sent = match chunk {
            Chunk::Ring(lease) if taken + count == len => {
                let sent = self.lend(to, head, lease, taken..len, now);
                self.arm_recv(from);
                sent
            }
            chunk => self.copy_forward((from, to), head, (chunk, taken, len), count, now),
        };
        self.forwarded(from, sent)
    }

    fn forwarded(&self, from: Handle, sent: Sent) -> Forwarded {
        let left = match self.peek(from, <[u8]>::len) {
            Peek::Held(left) => left,
            _ => 0,
        };
        Forwarded { sent, left }
    }

    /// Drops the first `count` unread bytes of a socket and returns how many are left.
    pub fn skip(&mut self, handle: Handle, count: usize) -> usize {
        let Some((chunk, taken, len)) = self.unpark(handle) else {
            return 0;
        };
        let taken = (taken + count).min(len);
        self.consumed(handle, (chunk, taken, len));
        len - taken
    }

    fn unpark(&mut self, handle: Handle) -> Option<(Chunk, usize, usize)> {
        let slot = self.slot(handle)?;
        match std::mem::replace(&mut slot.reader, Reader::Idle) {
            Reader::Parked { chunk, taken, len } => Some((chunk, taken, len)),
            reader => {
                slot.reader = reader;
                None
            }
        }
    }

    fn copy_forward(
        &mut self,
        (from, to): (Handle, Handle),
        head: &[u8],
        (chunk, taken, len): (Chunk, usize, usize),
        count: usize,
        now: bool,
    ) -> Sent {
        let mut body = self.take_staging();
        body.resize(count, 0);
        self.copy_out(&chunk, taken, &mut body);
        let sent = self.send_with(to, (head, &body), now);
        self.give_staging(body);
        let moved = match sent {
            Sent::Queued(written) => written.saturating_sub(head.len()),
            Sent::Busy | Sent::Failed => 0,
        };
        self.consumed(from, (chunk, taken + moved, len));
        sent
    }

    fn lend(
        &mut self,
        to: Handle,
        head: &[u8],
        lease: Lease,
        range: Range<usize>,
        now: bool,
    ) -> Sent {
        let total = head.len() + range.len();
        let written = if now {
            let body = self
                .queue
                .held(lease.pool)
                .view(lease.buffer, range.start, range.len());
            self.write_now(to, head, body)
        } else {
            Some(0)
        };
        let Some(written) = written else {
            self.give_buffer(lease);
            return self.send_broke(to);
        };
        if written == total {
            self.give_buffer(lease);
            return Sent::Queued(total);
        }
        let skipped = written.min(head.len());
        let rest = range.start + (written - skipped)..range.end;
        if self.launch_lent(to, &head[skipped..], lease, rest).is_err() {
            self.give_buffer(lease);
            return self.send_broke(to);
        }
        Sent::Queued(total)
    }

    fn launch_lent(
        &mut self,
        handle: Handle,
        head: &[u8],
        lease: Lease,
        range: Range<usize>,
    ) -> std::io::Result<()> {
        let meta = Self::meta(Kind::Send, 0, handle);
        let mut staging = self.take_staging();
        staging.extend_from_slice(head);
        let key = self
            .with_fd(handle, |queue, fd| {
                queue.send_lent(fd, (staging, lease, range), meta)
            })
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))??;
        self.flying(handle, key);
        Ok(())
    }
}
