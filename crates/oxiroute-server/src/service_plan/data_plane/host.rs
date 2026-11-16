use std::sync::OnceLock;

use oxiroute_config::HttpDataPlane;

use super::{DataPlane, MissingCapability, PlaneCapabilities, chain::order};

type Capability = Result<(), MissingCapability>;

impl PlaneCapabilities {
    pub(crate) fn detect(setting: Option<HttpDataPlane>) -> Self {
        let reachable = order(setting, DataPlane::DEFAULT_ORDER);
        let detect = |plane, probe: fn() -> Capability| reachable.contains(&plane).then(probe);
        Self {
            epoll: detect(DataPlane::Epoll, epoll),
            io_uring: detect(DataPlane::IoUring, io_uring),
        }
    }
}

fn epoll() -> Capability {
    static DETECTED: OnceLock<Capability> = OnceLock::new();
    DETECTED
        .get_or_init(|| {
            oxiroute_epoll_h1::supported()
                .then_some(())
                .ok_or_else(|| MissingCapability("epoll is not available on this platform".into()))
        })
        .clone()
}

#[cfg(target_os = "linux")]
fn probe_io_uring() -> Capability {
    oxiroute_epoll_h1::io_uring_support().map_err(|missing| MissingCapability(missing.to_string()))
}

#[cfg(not(target_os = "linux"))]
fn probe_io_uring() -> Capability {
    Err(MissingCapability("only Linux provides io_uring".into()))
}

fn io_uring() -> Capability {
    static DETECTED: OnceLock<Capability> = OnceLock::new();
    DETECTED.get_or_init(probe_io_uring).clone()
}
