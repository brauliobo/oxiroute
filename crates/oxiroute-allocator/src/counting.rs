//! A jemalloc that counts its calls, for `oxiroute --features alloc-count`: the allocation ceilings of the lane tests
//! and the allocator columns of the data-path harness. Counting costs one relaxed add per call.

use std::{
    alloc::{GlobalAlloc, Layout},
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

use tikv_jemallocator::Jemalloc;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

pub struct CountingJemalloc;

// SAFETY: every call is forwarded unchanged to jemalloc with the caller's layout and pointers.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingJemalloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { Jemalloc.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { Jemalloc.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        FREES.fetch_add(1, Relaxed);
        unsafe { Jemalloc.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(size);
        unsafe { Jemalloc.realloc(pointer, layout, size) }
    }
}

fn count(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Relaxed);
    BYTES.fetch_add(bytes as u64, Relaxed);
}

/// One snapshot line, in the field order of the harness shim `tool/alloc_count.c`: allocations (allocations, zeroed
/// allocations, reallocations and aligned allocations are one count here, the other three fields are zero), frees,
/// requested bytes, then zeros for the clock and the live bytes.
#[must_use]
pub fn snapshot() -> String {
    format!("{} 0 0 0 {} {} 0 0", ALLOCATIONS.load(Relaxed), FREES.load(Relaxed), BYTES.load(Relaxed))
}
