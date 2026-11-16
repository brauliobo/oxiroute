use std::{io, net::SocketAddr, os::fd::OwnedFd, time::Duration};

use oxiroute_uring::Released;
use rustix::{
    buffer::spare_capacity,
    event::{
        Timespec,
        epoll::{self, Event, EventFlags},
    },
    io::Errno,
};

use super::Polled;
use crate::plane::{
    config::{Config, Trigger},
    link::{Io, Link},
    slab::Token,
    sys,
};

pub(crate) struct Epoll {
    fd: OwnedFd,
    trigger: Trigger,
    events: Vec<Event>,
}

fn timespec(duration: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(duration.subsec_nanos()),
    }
}

impl Epoll {
    pub(crate) fn new(config: &Config) -> io::Result<Self> {
        Ok(Self {
            fd: epoll::create(epoll::CreateFlags::CLOEXEC)?,
            trigger: config.trigger,
            events: Vec::with_capacity(config.events),
        })
    }

    pub(crate) fn wait(
        &mut self,
        timeout: Option<Duration>,
        out: &mut Vec<Polled>,
    ) -> io::Result<()> {
        self.events.clear();
        let timeout = timeout.map(timespec);
        match epoll::wait(&self.fd, spare_capacity(&mut self.events), timeout.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
        out.extend(
            self.events
                .iter()
                .map(|event| Polled::Ready(Token::from_u64(event.data.u64()), event.flags)),
        );
        Ok(())
    }

    pub(crate) fn open(&self, fd: OwnedFd, token: Token) -> io::Result<Link> {
        let flags = sys::interest_flags(self.trigger, true, true);
        sys::watch(&self.fd, &fd, token.to_u64(), flags)?;
        Ok(Link::new(Io::Fd(fd)))
    }

    pub(crate) fn dial(&self, token: Token, address: SocketAddr) -> io::Result<(Link, bool)> {
        let connecting = sys::connect(&address)?;
        Ok((self.open(connecting.fd, token)?, connecting.established))
    }

    pub(crate) fn release(&self, link: Link) -> io::Result<Released> {
        let released = link.release()?;
        sys::unwatch(&self.fd, &released.fd);
        Ok(released)
    }

    pub(crate) fn serve_listener(
        &self,
        fd: &OwnedFd,
        token: Token,
        serving: bool,
    ) -> io::Result<()> {
        if serving {
            let flags = EventFlags::IN | EventFlags::EXCLUSIVE;
            sys::watch(&self.fd, fd, token.to_u64(), flags)
        } else {
            sys::unwatch(&self.fd, fd);
            Ok(())
        }
    }

    pub(crate) fn watch_wake(&self, token: Token, fd: &OwnedFd) -> io::Result<()> {
        sys::watch(&self.fd, fd, token.to_u64(), EventFlags::IN)
    }

    pub(crate) fn level_triggered(&self) -> bool {
        self.trigger == Trigger::Level
    }

    pub(crate) fn rewatch(&self, token: Token, link: &mut Link, interest: (bool, bool)) {
        if link.interest == interest {
            return;
        }
        link.interest = interest;
        if let Io::Fd(fd) = &link.io {
            let flags = sys::interest_flags(self.trigger, interest.0, interest.1);
            let _ = sys::rewatch(&self.fd, fd, token.to_u64(), flags);
        }
    }
}
