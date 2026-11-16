//! Thread-per-core epoll data plane for plain HTTP/1.1 proxy exchanges (`docs/developer/design/EPOLL_DATA_PLANE.md`).

#[cfg(target_os = "linux")]
mod plane;

#[cfg(target_os = "linux")]
pub use plane::*;

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
