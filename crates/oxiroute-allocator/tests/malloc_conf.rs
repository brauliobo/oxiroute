//! The embedded configuration is what jemalloc reports, with no environment variable set, in a binary that declares
//! the allocator the way `oxiroute` does; the purging thread starts on request.
#![cfg(target_os = "linux")]
#![allow(unsafe_code)]

use std::{ffi::c_void, ptr};

#[global_allocator]
static ALLOCATOR: oxiroute_allocator::Jemalloc = oxiroute_allocator::Jemalloc;

fn option<T: Default>(name: &[u8]) -> T {
    let mut value = T::default();
    let mut size = size_of::<T>();
    // SAFETY: `name` is NUL-terminated and `value` is a `T` of the `size` jemalloc writes for this option.
    let status = unsafe {
        tikv_jemalloc_sys::mallctl(
            name.as_ptr().cast(),
            ptr::from_mut(&mut value).cast::<c_void>(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!(status, 0, "mallctl {}", String::from_utf8_lossy(name));
    value
}

#[test]
fn jemalloc_starts_no_thread_of_its_own_until_asked_to() {
    assert!(
        std::env::var_os("_RJEM_MALLOC_CONF").is_none(),
        "the run-time override would hide the embedded value"
    );
    assert!(!option::<bool>(b"opt.background_thread\0"));
    assert!(!option::<bool>(b"background_thread\0"));
    oxiroute_allocator::start_background_thread().expect("start the purging thread");
    assert!(option::<bool>(b"background_thread\0"));
}

#[test]
fn jemalloc_reports_the_embedded_thread_cache_sizes() {
    assert!(
        std::env::var_os("_RJEM_MALLOC_CONF").is_none(),
        "the run-time override would hide the embedded value"
    );
    assert_eq!(option::<u32>(b"opt.tcache_nslots_small_min\0"), 400);
    assert_eq!(option::<u32>(b"opt.tcache_nslots_small_max\0"), 2000);
    assert_eq!(option::<u32>(b"opt.tcache_nslots_large\0"), 400);
    assert_eq!(option::<usize>(b"opt.tcache_max\0"), 1 << 17);
}

fn resident_bytes() -> usize {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("statm");
    let pages: usize = statm
        .split_whitespace()
        .nth(1)
        .expect("resident pages")
        .parse()
        .expect("number");
    pages * 4096
}

#[test]
fn unused_memory_is_returned_when_asked_for() {
    let baseline = resident_bytes();
    let blocks: Vec<Vec<u8>> = (0..64_u8).map(|index| vec![index | 1; 1 << 20]).collect();
    let held = resident_bytes();
    assert!(
        held > baseline + (48 << 20),
        "the blocks are resident ({baseline} before, {held} held)"
    );
    drop(blocks);

    oxiroute_allocator::release_unused();

    let released = resident_bytes();
    assert!(
        released < baseline + (16 << 20),
        "the pages of 64 freed blocks are returned at once ({baseline} before, {released} after)"
    );
}
