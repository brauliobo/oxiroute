use std::{cell::RefCell, io, net::SocketAddr, os::fd::OwnedFd, rc::Rc, time::Duration};

use oxiroute_uring::{Event, Handle, Read as Received, Released, Ring, Sent};
use rustix::event::epoll::EventFlags;

use super::Polled;
use crate::plane::{
    config::Config,
    link::{Io, Link},
    slab::Token,
    sys::{self, Read, Wrote},
};

type Shared = Rc<RefCell<Ring>>;

/// A socket owned by the thread's ring. Dropping it closes the socket the way the ring defines: receives are
/// cancelled, a send that was accepted finishes, the descriptor closes when nothing uses it any more.
pub(crate) struct RingSocket {
    ring: Shared,
    handle: Handle,
}

impl Drop for RingSocket {
    fn drop(&mut self) {
        self.ring.borrow_mut().close(self.handle);
    }
}

impl RingSocket {
    pub(crate) fn read(&self, buf: &mut [u8]) -> Read {
        match self.ring.borrow_mut().read(self.handle, buf) {
            Received::Data(count) => Read::Data(count),
            Received::Eof => Read::Eof,
            Received::Block => Read::Block,
            Received::Failed => Read::Failed,
        }
    }

    pub(crate) fn peek(&self) -> Read {
        match self.ring.borrow_mut().peek(self.handle) {
            Received::Data(count) => Read::Data(count),
            Received::Eof => Read::Eof,
            Received::Block => Read::Block,
            Received::Failed => Read::Failed,
        }
    }

    pub(crate) fn write(&self, head: &[u8], body: &[u8]) -> Wrote {
        match self.ring.borrow_mut().send(self.handle, head, body) {
            Sent::Queued(count) => Wrote::Bytes(count),
            Sent::Busy => Wrote::Block,
            Sent::Failed => Wrote::Failed,
        }
    }

    pub(crate) fn shutdown_write(&self) {
        self.ring.borrow_mut().shutdown_write(self.handle);
    }

    pub(crate) fn connected(&self) -> bool {
        self.ring.borrow_mut().connected(self.handle) == Some(true)
    }

    pub(crate) fn release(self) -> io::Result<Released> {
        self.ring.borrow_mut().release(self.handle)
    }
}

pub(crate) struct Uring {
    ring: Shared,
    listeners: Vec<(Token, Handle)>,
    wakes: Vec<(Token, Handle)>,
    events: Vec<Event>,
}

fn polled(event: Event) -> Polled {
    let ready = |tag, flags| Polled::Ready(Token::from_u64(tag), flags);
    match event {
        Event::Readable { tag } | Event::Notified { tag } => ready(tag, EventFlags::IN),
        Event::Eof { tag } => ready(tag, EventFlags::IN | EventFlags::RDHUP),
        Event::Failed { tag } => ready(tag, EventFlags::ERR | EventFlags::HUP),
        Event::Writable { tag } | Event::Connected { tag } => ready(tag, EventFlags::OUT),
        Event::Accepted { tag, fd, peer } => Polled::Accepted(Token::from_u64(tag), fd, peer),
    }
}

impl Uring {
    pub(crate) fn new(config: &Config) -> io::Result<Self> {
        Ok(Self {
            ring: Rc::new(RefCell::new(Ring::new(&config.uring)?)),
            listeners: Vec::new(),
            wakes: Vec::new(),
            events: Vec::with_capacity(config.events),
        })
    }

    pub(crate) fn wait(
        &mut self,
        timeout: Option<Duration>,
        out: &mut Vec<Polled>,
    ) -> io::Result<()> {
        self.events.clear();
        self.ring.borrow_mut().wait(timeout, &mut self.events)?;
        out.extend(self.events.drain(..).map(polled));
        Ok(())
    }

    fn link(&self, handle: Handle) -> Link {
        let ring = Rc::clone(&self.ring);
        Link::new(Io::Ring(RingSocket { ring, handle }))
    }

    pub(crate) fn open(&self, fd: OwnedFd, token: Token, secure: bool) -> io::Result<Link> {
        if secure {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the io_uring driver cannot terminate TLS",
            ));
        }
        let handle = {
            let mut ring = self.ring.borrow_mut();
            let handle = ring.adopt(fd, token.to_u64());
            ring.start(handle);
            handle
        };
        Ok(self.link(handle))
    }

    pub(crate) fn dial(&self, token: Token, address: SocketAddr) -> io::Result<(Link, bool)> {
        let fd = sys::unconnected(&address)?;
        let handle = {
            let mut ring = self.ring.borrow_mut();
            let handle = ring.adopt(fd, token.to_u64());
            ring.connect(handle, address);
            handle
        };
        Ok((self.link(handle), false))
    }

    pub(crate) fn add_listener(&mut self, token: Token, fd: &OwnedFd) -> io::Result<()> {
        let handle = self
            .ring
            .borrow_mut()
            .listen(fd.try_clone()?, token.to_u64());
        self.listeners.push((token, handle));
        Ok(())
    }

    pub(crate) fn remove_listener(&mut self, token: Token) {
        if let Some(at) = self
            .listeners
            .iter()
            .position(|(listener, _)| *listener == token)
        {
            let (_, handle) = self.listeners.swap_remove(at);
            self.ring.borrow_mut().close(handle);
        }
    }

    pub(crate) fn set_listening(&self, token: Token, on: bool) {
        let listener = self
            .listeners
            .iter()
            .find(|(listener, _)| *listener == token);
        if let Some((_, handle)) = listener {
            self.ring.borrow_mut().set_accepting(*handle, on);
        }
    }

    pub(crate) fn watch_wake(&mut self, token: Token, fd: &OwnedFd) -> io::Result<()> {
        let mut ring = self.ring.borrow_mut();
        let handle = ring.adopt(fd.try_clone()?, token.to_u64());
        ring.notify(handle);
        self.wakes.push((token, handle));
        Ok(())
    }

    pub(crate) fn wake_consumed(&self, token: Token) {
        let wake = self.wakes.iter().find(|(wake, _)| *wake == token);
        if let Some((_, handle)) = wake {
            self.ring.borrow_mut().notify(*handle);
        }
    }
}
