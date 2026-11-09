//! Per-thread pool of 128 KiB body buffers.
//!
//! A large HTTP/1 body used to be read into a reader-owned buffer and then copied into a new `Bytes` per chunk,
//! one allocation and one copy of the chunk size each. Here the buffer itself is handed out as the chunk
//! ([`freeze`]) and returns to the pool of the thread that drops the last clone, so a stream of large chunks
//! reuses the same few buffers and the allocator is not involved.

use std::cell::RefCell;

use bytes::{Bytes, BytesMut};

/// Size of every pooled buffer, and so the largest body chunk one socket read can yield. A body that arrives faster
/// than it is forwarded takes one read, queue hop and write per chunk, so a larger chunk costs fewer instructions per
/// byte; it also sets how much memory a slow download holds while its chunks are queued.
pub const CHUNK_BYTES: usize = 128 * 1024;
/// Buffers kept per thread when idle (1 MiB).
const POOL_MAX: usize = 8;

thread_local! {
    static POOL: RefCell<Vec<BytesMut>> = const { RefCell::new(Vec::new()) };
}

/// A buffer of [`CHUNK_BYTES`] initialised bytes, taken from the pool when one is available.
pub fn take() -> BytesMut {
    POOL.with_borrow_mut(Vec::pop)
        .unwrap_or_else(|| BytesMut::zeroed(CHUNK_BYTES))
}

/// Puts a buffer back unless it was shrunk or split (only whole buffers keep their initialised length).
pub fn recycle(buf: BytesMut) {
    if buf.len() == CHUNK_BYTES && buf.capacity() == CHUNK_BYTES {
        POOL.with_borrow_mut(|pool| {
            if pool.len() < POOL_MAX {
                pool.push(buf);
            }
        });
    }
}

/// Hands out `buf[start..end]` without copying; the buffer is recycled when the last clone is dropped.
pub fn freeze(buf: BytesMut, start: usize, end: usize) -> Bytes {
    Bytes::from_owner(Chunk { buf, start, end })
}

struct Chunk {
    buf: BytesMut,
    start: usize,
    end: usize,
}

impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        recycle(std::mem::take(&mut self.buf));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_chunk_returns_its_buffer() {
        let mut buf = take();
        buf[..3].copy_from_slice(b"abc");
        let pointer = buf.as_ptr();
        let chunk = freeze(buf, 0, 3);
        let tail = chunk.slice(1..);
        drop(chunk);
        assert_eq!(&tail[..], b"bc");
        drop(tail);
        let again = take();
        assert_eq!(again.as_ptr(), pointer);
        assert_eq!(again.len(), CHUNK_BYTES);
    }

    #[test]
    fn a_shrunk_buffer_is_not_pooled() {
        let mut buf = take();
        buf.truncate(10);
        recycle(buf);
        assert_eq!(POOL.with_borrow(Vec::len), 0);
    }

    #[test]
    fn the_pool_is_bounded() {
        let buffers: Vec<_> = (0..POOL_MAX + 4).map(|_| take()).collect();
        buffers.into_iter().for_each(recycle);
        assert_eq!(POOL.with_borrow(Vec::len), POOL_MAX);
        POOL.with_borrow_mut(Vec::clear);
    }
}
