//! The connection output queue: one contiguous buffer for small frames plus separate segments for large DATA
//! payloads, drained with vectored writes.
use std::collections::VecDeque;
use std::io::IoSlice;

use bytes::{Buf, Bytes, BytesMut};

/// Payloads up to this size are copied behind their frame header; larger ones become segments.
pub const COPY_LIMIT: usize = 4096;
const MAX_SLICES: usize = 16;

#[derive(Default)]
pub struct Output {
    /// Frozen prefix: segments that precede `tail` on the wire.
    chunks: VecDeque<Bytes>,
    /// Small frames appended since the last segment.
    pub tail: BytesMut,
}

impl Output {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tail.is_empty() && self.chunks.is_empty()
    }

    /// Queues a large payload after everything queued so far without copying it.
    pub fn push_segment(&mut self, data: Bytes) {
        if !self.tail.is_empty() {
            self.chunks.push_back(self.tail.split().freeze());
        }
        self.chunks.push_back(data);
    }

    /// Fills `slices` with the front of the queue; returns how many were set.
    pub fn slices<'a>(&'a self, slices: &mut [IoSlice<'a>; MAX_SLICES]) -> usize {
        let mut n = 0;
        for chunk in self.chunks.iter().take(MAX_SLICES - 1) {
            slices[n] = IoSlice::new(chunk);
            n += 1;
        }
        // The tail follows the chunks on the wire: it may only be listed when every chunk was.
        if !self.tail.is_empty() && self.chunks.len() < MAX_SLICES {
            slices[n] = IoSlice::new(&self.tail);
            n += 1;
        }
        n
    }

    /// Drops `n` written octets from the front.
    pub fn advance(&mut self, mut n: usize) {
        while n > 0 {
            match self.chunks.front_mut() {
                Some(front) if front.len() <= n => {
                    n -= front.len();
                    self.chunks.pop_front();
                }
                Some(front) => {
                    front.advance(n);
                    return;
                }
                None => {
                    self.tail.advance(n);
                    return;
                }
            }
        }
    }

    /// Releases the buffer when it is empty and larger than `limit`, so an idle connection keeps little.
    pub fn shrink(&mut self, limit: usize) {
        if self.is_empty() && self.tail.capacity() > limit {
            self.tail = BytesMut::new();
        }
    }

    /// Octets queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tail.len() + self.chunks.iter().map(Bytes::len).sum::<usize>()
    }
}

#[must_use]
pub fn empty_slices() -> [IoSlice<'static>; MAX_SLICES] {
    [IoSlice::new(&[]); MAX_SLICES]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(o: &Output) -> Vec<u8> {
        let mut s = empty_slices();
        let n = o.slices(&mut s);
        s[..n].iter().flat_map(|s| s.iter().copied()).collect()
    }

    #[test]
    fn segments_keep_wire_order_and_advance_across_them() {
        let mut o = Output::default();
        o.tail.extend_from_slice(b"ab");
        o.push_segment(Bytes::from_static(b"CDEF"));
        o.tail.extend_from_slice(b"gh");
        assert_eq!(wire(&o), b"abCDEFgh");
        o.advance(3);
        assert_eq!(wire(&o), b"DEFgh");
        o.advance(5);
        assert!(o.is_empty());
    }

    #[test]
    fn tail_is_not_listed_ahead_of_unlisted_chunks() {
        let mut o = Output::default();
        for i in 0..20u8 {
            o.push_segment(Bytes::from(vec![i]));
        }
        o.tail.extend_from_slice(b"tail");
        let mut seen = Vec::new();
        while !o.is_empty() {
            let mut s = empty_slices();
            let n = o.slices(&mut s);
            let got: Vec<u8> = s[..n].iter().flat_map(|s| s.iter().copied()).collect();
            o.advance(got.len());
            seen.extend(got);
        }
        let mut expected: Vec<u8> = (0..20).collect();
        expected.extend_from_slice(b"tail");
        assert_eq!(seen, expected);
    }
}
