//! Global allocator of the oxiroute binary and its embedded jemalloc configuration.

#[cfg(target_os = "linux")]
pub use tikv_jemallocator::Jemalloc;

/// jemalloc reads its configuration from this NUL-terminated string (a thin pointer, so the array length is part of
/// the type). Bigger per-thread caches keep the buffers of HTTP/2 and connection-per-request traffic out of the arenas:
/// -2% to -5% instructions on those lanes. `_RJEM_MALLOC_CONF` at run time still overrides every key.
///
/// `background_thread` is not set here: jemalloc would start its thread with the first allocation, and a supervised
/// worker must still be single-threaded when it adopts its control endpoint. See [`start_background_thread`].
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
#[unsafe(export_name = "_rjem_malloc_conf")]
pub static MALLOC_CONF: &[u8; 99] =
    b"tcache_nslots_small_min:400,tcache_nslots_small_max:2000,tcache_nslots_large:400,tcache_max:131072\0";

/// Starts the jemalloc background thread that returns dirty pages when their decay time is up. Without it jemalloc only
/// purges while some thread allocates, so a process that went idle after a burst (thousands of connections closed)
/// kept the memory of the burst until the next one. Call it once the process no longer needs to be single-threaded.
///
/// # Errors
///
/// Returns the error of `mallctl("background_thread")`.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn start_background_thread() -> std::io::Result<()> {
    let mut enable = true;
    // SAFETY: the name is NUL-terminated and `background_thread` is a `bool` that jemalloc reads from `enable`.
    let status = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"background_thread".as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::from_mut(&mut enable).cast(),
            size_of::<bool>(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status))
    }
}
