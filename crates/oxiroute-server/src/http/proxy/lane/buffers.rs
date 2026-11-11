//! Read and write buffers of the lane: an idle connection holds none. A buffer is taken from a per-thread pool when
//! a request starts to arrive and goes back when the request is served, so the buffers a thread needs are the ones
//! its busy connections use at the same time.

use std::cell::RefCell;

use bytes::BytesMut;
use oxiroute_h1_head::{RequestHead, ResponseHead};

const READ_CAPACITY: usize = 8 * 1024;
const POOLED: usize = 64;
const WRITE_CAPACITY: usize = 1024;

thread_local! {
    static READ: RefCell<Vec<BytesMut>> = const { RefCell::new(Vec::new()) };
    // The heads are large and move in and out of the pool by pointer.
    #[allow(clippy::vec_box)]
    static HEADS: RefCell<Vec<Box<RequestHead>>> = const { RefCell::new(Vec::new()) };
    #[allow(clippy::vec_box)]
    static RESPONSES: RefCell<Vec<Box<ResponseHead>>> = const { RefCell::new(Vec::new()) };
    static WRITE: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
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

/// A scanned-head record, kept per thread because it is large and a request holds one only while it is served.
pub(super) fn take_head() -> Box<RequestHead> {
    HEADS
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| Box::new(RequestHead::new()))
}

pub(super) fn give_head(head: Box<RequestHead>) {
    HEADS.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(head);
        }
    });
}

/// A buffer a head is rendered into.
pub(super) fn take_write() -> Vec<u8> {
    WRITE
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| Vec::with_capacity(WRITE_CAPACITY))
}

pub(super) fn give_write(mut buffer: Vec<u8>) {
    if buffer.capacity() == 0 || buffer.capacity() > 4 * WRITE_CAPACITY {
        return;
    }
    buffer.clear();
    WRITE.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(buffer);
        }
    });
}

/// A scanned response head, kept per thread for the same reason.
pub(super) fn take_response_head() -> Box<ResponseHead> {
    RESPONSES
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| Box::new(ResponseHead::new()))
}

pub(super) fn give_response_head(head: Box<ResponseHead>) {
    RESPONSES.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(head);
        }
    });
}
