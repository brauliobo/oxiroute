mod epoll;
mod ring;

use std::{io, net::SocketAddr, os::fd::OwnedFd, time::Duration};

use oxiroute_uring::Released;
use rustix::event::epoll::EventFlags;
use rustls::ServerConnection;

use self::ring::Uring;
pub(crate) use self::{epoll::Epoll, ring::RingSocket};
use super::{
    config::{Config, Driver as Kind},
    link::Link,
    slab::Token,
    tls::Session,
};

pub(crate) enum Polled {
    Ready(Token, EventFlags),
    Accepted(Token, OwnedFd, Option<SocketAddr>),
}

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

    pub(crate) fn check(config: &Config, terminates_tls: bool) -> io::Result<()> {
        let refusal = match config.driver {
            Kind::IoUring if terminates_tls => "the io_uring driver cannot terminate TLS",
            Kind::IoUring if config.scheduling.is_some() => {
                "the io_uring driver does not serve per-worker scheduling"
            }
            _ => return Ok(()),
        };
        Err(io::Error::new(io::ErrorKind::InvalidInput, refusal))
    }

    pub(crate) const fn splices(&self) -> bool {
        matches!(self, Self::Epoll(_))
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

    pub(crate) fn open(
        &self,
        fd: OwnedFd,
        token: Token,
        session: Option<Session>,
    ) -> io::Result<Link> {
        match self {
            Self::Epoll(driver) => driver.open(fd, token, session),
            Self::Uring(driver) => driver.open(fd, token, session.is_some()),
        }
    }

    pub(crate) fn dial(&self, token: Token, address: SocketAddr) -> io::Result<(Link, bool)> {
        match self {
            Self::Epoll(driver) => driver.dial(token, address),
            Self::Uring(driver) => driver.dial(token, address),
        }
    }

    pub(crate) fn release(
        &self,
        link: Link,
    ) -> io::Result<(Released, Option<Box<ServerConnection>>)> {
        match self {
            Self::Epoll(driver) => driver.release(link),
            Self::Uring(_) => link.into_parts(),
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

    pub(crate) fn set_listening(&self, token: Token, fd: &OwnedFd, on: bool) -> io::Result<()> {
        match self {
            Self::Epoll(driver) => driver.set_listening(token, fd, on),
            Self::Uring(driver) => {
                driver.set_listening(token, on);
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

    pub(crate) fn wake_consumed(&self, token: Token) {
        if let Self::Uring(driver) = self {
            driver.wake_consumed(token);
        }
    }
}
