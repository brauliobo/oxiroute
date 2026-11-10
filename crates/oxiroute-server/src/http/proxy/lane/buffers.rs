//! Read and write buffers of the lane: an idle connection holds none. A buffer is taken from a per-thread pool when
//! a request starts to arrive and goes back when the request is served, so the buffers a thread needs are the ones
//! its busy connections use at the same time.

use std::cell::RefCell;

use bytes::BytesMut;

const READ_CAPACITY: usize = 8 * 1024;
const POOLED: usize = 64;

thread_local! {
    static READ: RefCell<Vec<BytesMut>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn take_read() -> BytesMut {
    READ.with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| BytesMut::with_capacity(READ_CAPACITY))
}

/// Keeps `buffer` for the next request of any connection of this thread; a buffer that grew is dropped.
pub(super) fn give_read(mut buffer: BytesMut) {
    if buffer.capacity() < READ_CAPACITY || buffer.capacity() > 2 * READ_CAPACITY {
        return;
    }
    buffer.clear();
    READ.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(buffer);
        }
    });
}
