//! Read buffers: an idle connection holds none. The driver takes one from a small per-thread pool when it
//! starts a read and puts it back when the read finds nothing, so a steady connection does not allocate per read.
use std::cell::RefCell;

use bytes::BytesMut;

/// Default read size and the capacity of pooled buffers.
pub const READ_SIZE: usize = 16 * 1024 + 64;
const POOL_MAX: usize = 8;

thread_local! {
    static POOL: RefCell<Vec<BytesMut>> = const { RefCell::new(Vec::new()) };
}

#[must_use]
pub fn take() -> BytesMut {
    POOL.with(|p| p.borrow_mut().pop())
        .unwrap_or_else(|| BytesMut::with_capacity(READ_SIZE))
}

/// Returns an empty buffer to the pool; oversized or exhausted ones are dropped.
pub fn give(mut buf: BytesMut) {
    buf.clear();
    if buf.capacity() >= READ_SIZE / 2 && buf.capacity() <= 2 * READ_SIZE {
        POOL.with(|p| {
            let mut p = p.borrow_mut();
            if p.len() < POOL_MAX {
                p.push(buf);
            }
        });
    }
}
