use std::sync::OnceLock;

use oxiroute_config::{HttpDataPlane, ListenerBind, Protocol};
use thiserror::Error;

use super::{DataPlane, EngineConditions};

const _: () = assert!(matches!(
    DataPlane::DEFAULT_ORDER.last(),
    Some(DataPlane::Tokio)
));

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum Skip {
    #[error("{0}")]
    Unavailable(String),
    #[error("http_fast_lane is turned off")]
    FastLaneOff,
    #[error("connection_scheduling `aged_per_worker` is not supported by this plane")]
    Scheduling,
    #[error("only plain HTTP/1.1 listeners are served")]
    Protocol,
    #[error("only socket listeners are served")]
    Bind,
    #[error("TLS listeners are not served")]
    Tls,
    #[error("no route of the service is served")]
    Routes,
}

pub(crate) type Probe = fn() -> Option<String>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Link {
    pub(crate) plane: DataPlane,
    pub(crate) probe: Probe,
    pub(crate) engine: bool,
    pub(crate) scheduling: bool,
    pub(crate) tls: bool,
}

impl Link {
    pub(crate) const IO_URING: Self = Self {
        plane: DataPlane::IoUring,
        probe: io_uring_probe,
        engine: true,
        scheduling: false,
        tls: false,
    };
    pub(crate) const EPOLL: Self = Self {
        plane: DataPlane::Epoll,
        probe: epoll_probe,
        engine: true,
        scheduling: true,
        tls: false,
    };
    pub(crate) const TOKIO: Self = Self {
        plane: DataPlane::Tokio,
        probe: always,
        engine: false,
        scheduling: true,
        tls: true,
    };

    fn available(self, conditions: EngineConditions) -> Result<(), Skip> {
        if let Some(missing) = (self.probe)() {
            return Err(Skip::Unavailable(missing));
        }
        if !self.engine {
            return Ok(());
        }
        if !conditions.fast_lane {
            return Err(Skip::FastLaneOff);
        }
        if conditions.scheduling && !self.scheduling {
            return Err(Skip::Scheduling);
        }
        Ok(())
    }

    fn serves(self, listener: ListenerShape, routes: bool) -> Result<(), Skip> {
        if self.engine && !listener.http1 {
            return Err(Skip::Protocol);
        }
        if self.engine && !listener.socket {
            return Err(Skip::Bind);
        }
        if listener.tls && !self.tls {
            return Err(Skip::Tls);
        }
        if self.engine && !routes {
            return Err(Skip::Routes);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ListenerShape {
    http1: bool,
    socket: bool,
    tls: bool,
}

impl ListenerShape {
    pub(crate) fn of(protocol: Protocol, bind: &ListenerBind, tls: bool) -> Self {
        Self {
            http1: protocol == Protocol::Http,
            socket: matches!(bind, ListenerBind::Socket { .. }),
            tls,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    link: Link,
    available: Result<(), Skip>,
}

impl Candidate {
    pub(crate) const fn plane(&self) -> DataPlane {
        self.link.plane
    }

    pub(crate) fn unavailable(&self) -> Option<&Skip> {
        self.available.as_ref().err()
    }

    pub(crate) fn admits(&self, listener: ListenerShape, routes: bool) -> Result<(), Skip> {
        self.available.clone()?;
        self.link.serves(listener, routes)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Planes {
    pub(crate) catalog: [Link; 3],
    pub(crate) order: &'static [DataPlane],
}

pub(crate) const PLANES: Planes = Planes {
    catalog: [Link::IO_URING, Link::EPOLL, Link::TOKIO],
    order: DataPlane::DEFAULT_ORDER,
};

impl Planes {
    pub(crate) fn candidates(
        self,
        setting: Option<HttpDataPlane>,
        conditions: EngineConditions,
    ) -> Vec<Candidate> {
        order(setting, self.order)
            .iter()
            .map(|plane| self.link(*plane))
            .map(|link| Candidate {
                link,
                available: link.available(conditions),
            })
            .collect()
    }

    fn link(self, plane: DataPlane) -> Link {
        *self
            .catalog
            .iter()
            .find(|link| link.plane == plane)
            .expect("the catalog holds every plane")
    }
}

fn order(setting: Option<HttpDataPlane>, default: &'static [DataPlane]) -> &'static [DataPlane] {
    match setting {
        None => default,
        Some(HttpDataPlane::IoUring) => &[DataPlane::IoUring, DataPlane::Tokio],
        Some(HttpDataPlane::Epoll) => &[DataPlane::Epoll, DataPlane::Tokio],
        Some(HttpDataPlane::Tokio) => &[DataPlane::Tokio],
    }
}

fn always() -> Option<String> {
    None
}

fn epoll_probe() -> Option<String> {
    (!oxiroute_epoll_h1::supported()).then(|| "epoll is not available on this platform".to_owned())
}

#[cfg(target_os = "linux")]
fn probe_io_uring() -> Option<String> {
    oxiroute_epoll_h1::io_uring_support()
        .err()
        .map(|missing| missing.to_string())
}

#[cfg(not(target_os = "linux"))]
fn probe_io_uring() -> Option<String> {
    Some("only Linux provides io_uring".to_owned())
}

fn io_uring_probe() -> Option<String> {
    static MISSING: OnceLock<Option<String>> = OnceLock::new();
    MISSING.get_or_init(probe_io_uring).clone()
}
