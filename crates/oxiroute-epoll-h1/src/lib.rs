//! Thread-per-core epoll data plane for plain HTTP/1.1 proxy exchanges (`docs/developer/design/EPOLL_DATA_PLANE.md`).

#[cfg(target_os = "linux")]
mod plane;

#[cfg(target_os = "linux")]
pub use plane::*;

/// What the `io_uring` driver needs and this kernel or sandbox may lack; the error names the missing capability.
#[cfg(target_os = "linux")]
pub use oxiroute_uring::Missing;

/// Checks that the `io_uring` driver can run here with its default settings.
#[cfg(target_os = "linux")]
pub fn io_uring_support() -> Result<(), Missing> {
    oxiroute_uring::probe(&oxiroute_uring::Config::default())
}

#[cfg(target_os = "linux")]
#[must_use]
pub fn supported() -> bool {
    rustix::event::epoll::create(rustix::event::epoll::CreateFlags::CLOEXEC).is_ok()
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub const fn supported() -> bool {
    false
}
