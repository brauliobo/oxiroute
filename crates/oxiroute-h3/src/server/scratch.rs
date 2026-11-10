//! A per-thread output buffer for building frames without allocating per response.
use std::cell::RefCell;

/// Buffers above this size are not kept.
const KEEP_LIMIT: usize = 64 * 1024;

thread_local! {
    static SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Takes the thread's buffer, empty. The buffer is lost, not corrupted, when the caller is cancelled before
/// [`give`].
pub(super) fn take() -> Vec<u8> {
    let mut buffer = SCRATCH.with(RefCell::take);
    buffer.clear();
    buffer
}

pub(super) fn give(buffer: Vec<u8>) {
    if buffer.capacity() <= KEEP_LIMIT {
        SCRATCH.with(|slot| *slot.borrow_mut() = buffer);
    }
}
