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
