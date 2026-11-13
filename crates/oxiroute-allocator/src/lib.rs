//! Global allocator of the oxiroute binary and its embedded jemalloc configuration.

#[cfg(target_os = "linux")]
pub use tikv_jemallocator::Jemalloc;

/// jemalloc reads its configuration from this NUL-terminated string (a thin pointer, so the array length is part of
/// the type). Bigger per-thread caches keep the buffers of HTTP/2 and connection-per-request traffic out of the arenas:
/// -2% to -5% instructions on those lanes. A background thread returns dirty pages when their decay time is up: without
/// it jemalloc only purges while some thread allocates, so a process that went idle after a burst (thousands of
/// connections closed) kept the memory of the burst until the next one. `_RJEM_MALLOC_CONF` at run time still
/// overrides every key.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
#[unsafe(export_name = "_rjem_malloc_conf")]
pub static MALLOC_CONF: &[u8; 122] =
    b"background_thread:true,tcache_nslots_small_min:400,tcache_nslots_small_max:2000,tcache_nslots_large:400,tcache_max:131072\0";

/// Returns the unused pages of every arena to the kernel now. A process frees a lot at once when a generation ends
/// (the plans, pools and tables of a configuration that was replaced) and jemalloc would keep those pages, resident,
/// until their decay time is up; the end of a generation is the moment the process knows they are not coming back.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn release_unused() {
    // jemalloc's index of "every arena" in the name of an arena operation.
    const PURGE_ALL_ARENAS: &[u8] = b"arena.4096.purge\0";
    // SAFETY: the name is NUL-terminated and the operation takes and returns nothing.
    let status = unsafe {
        tikv_jemalloc_sys::mallctl(
            PURGE_ALL_ARENAS.as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    debug_assert_eq!(status, 0, "jemalloc refused to purge its arenas");
}

#[cfg(not(target_os = "linux"))]
pub fn release_unused() {}
