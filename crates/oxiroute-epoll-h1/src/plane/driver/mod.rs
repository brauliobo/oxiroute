mod epoll;
mod ring;

use std::{io, net::SocketAddr, os::fd::OwnedFd, time::Duration};

use oxiroute_uring::Released;
use rustix::event::epoll::EventFlags;

use self::ring::Uring;
pub(crate) use self::{epoll::Epoll, ring::RingSocket};
use super::{
    config::{Config, Driver as Kind},
    link::Link,
    slab::Token,
};

/// What a wait for events reports: readiness of a token, or a connection a listener accepted.
pub(crate) enum Polled {
    Ready(Token, EventFlags),
    Accepted(Token, OwnedFd, Option<SocketAddr>),
}

/// The I/O backend of one engine thread. The state machine above it reads and writes through [`Link`] and learns
/// what happened as [`Polled`] events; only this module knows whether that is `epoll` readiness or `io_uring`
/// completions.
pub(crate) enum Driver {
    Epoll(Epoll),
    Uring(Uring),
}

impl Driver {
    pub(crate) fn new(config: &Config) -> io::Result<Self> {
        match config.driver {
            Kind::Epoll => Epoll::new(config).map(Self::Epoll),
            Kind::IoUring => Uring::new(config).map(Self::Uring),
        }
    }

    pub(crate) fn wait(
        &mut self,
        timeout: Option<Duration>,
        out: &mut Vec<Polled>,
    ) -> io::Result<()> {
        match self {
            Self::Epoll(driver) => driver.wait(timeout, out),
            Self::Uring(driver) => driver.wait(timeout, out),
        }
    }

    /// Puts an established socket under the driver's watch.
    pub(crate) fn open(&self, fd: OwnedFd, token: Token) -> io::Result<Link> {
        match self {
            Self::Epoll(driver) => driver.open(fd, token),
            Self::Uring(driver) => Ok(driver.open(fd, token)),
        }
    }

    /// Starts a connection to `address`; the flag says it is already established.
    pub(crate) fn dial(&self, token: Token, address: SocketAddr) -> io::Result<(Link, bool)> {
        match self {
            Self::Epoll(driver) => driver.dial(token, address),
            Self::Uring(driver) => driver.dial(token, address),
        }
    }

    /// Takes a socket out of the driver, for a connection that leaves the data plane.
    pub(crate) fn release(&self, link: Link) -> io::Result<Released> {
        match self {
            Self::Epoll(driver) => driver.release(link),
            Self::Uring(_) => link.release(),
        }
    }

    pub(crate) fn level(&self) -> Option<&Epoll> {
        match self {
            Self::Epoll(driver) if driver.level_triggered() => Some(driver),
            _ => None,
        }
    }

    pub(crate) fn add_listener(&mut self, token: Token, fd: &OwnedFd) -> io::Result<()> {
        match self {
            Self::Epoll(_) => Ok(()),
            Self::Uring(driver) => driver.add_listener(token, fd),
        }
    }

    pub(crate) fn remove_listener(&mut self, token: Token) {
        if let Self::Uring(driver) = self {
            driver.remove_listener(token);
        }
    }

    pub(crate) fn serve_listener(
        &self,
        token: Token,
        fd: &OwnedFd,
        serving: bool,
    ) -> io::Result<()> {
        match self {
            Self::Epoll(driver) => driver.serve_listener(fd, token, serving),
            Self::Uring(driver) => {
                driver.serve_listener(token, serving);
                Ok(())
            }
        }
    }

    pub(crate) fn watch_wake(&mut self, token: Token, fd: &OwnedFd) -> io::Result<()> {
        match self {
            Self::Epoll(driver) => driver.watch_wake(token, fd),
            Self::Uring(driver) => driver.watch_wake(token, fd),
        }
    }

    /// Called after the wake descriptor was read, so the next write to it is reported again.
    pub(crate) fn wake_consumed(&self, token: Token) {
        if let Self::Uring(driver) = self {
            driver.wake_consumed(token);
        }
    }
}
