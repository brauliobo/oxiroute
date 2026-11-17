use std::{io, os::fd::OwnedFd};

use oxiroute_uring::{Peek, Released};
use rustix::event::epoll::EventFlags;

use super::{
    driver::RingSocket,
    sys::{self, Read, Wrote},
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ready {
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) hangup: bool,
    pub(crate) closing: bool,
}

impl Ready {
    pub(crate) fn apply(&mut self, flags: EventFlags) {
        let gone = flags.intersects(EventFlags::HUP | EventFlags::ERR);
        self.read |= gone || flags.intersects(EventFlags::IN | EventFlags::RDHUP);
        self.write |= gone || flags.contains(EventFlags::OUT);
        self.hangup |= gone;
        self.closing |= gone || flags.contains(EventFlags::RDHUP);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Delivery {
    Batched,
    Immediate,
}

impl Delivery {
    pub(crate) const fn of(keepalive: bool) -> Self {
        if keepalive {
            Self::Batched
        } else {
            Self::Immediate
        }
    }
}

pub(crate) enum Io {
    Fd(OwnedFd),
    Ring(RingSocket),
}

pub(crate) struct Link {
    pub(crate) io: Io,
    pub(crate) ready: Ready,
    pub(crate) interest: (bool, bool),
}

impl Link {
    pub(crate) const fn new(io: Io) -> Self {
        Self {
            io,
            ready: Ready {
                read: false,
                write: false,
                hangup: false,
                closing: false,
            },
            interest: (true, true),
        }
    }

    pub(crate) fn recv(&mut self, buf: &mut [u8]) -> Read {
        if !self.ready.read {
            return Read::Block;
        }
        let read = match &mut self.io {
            Io::Fd(fd) => sys::read(fd, buf),
            Io::Ring(socket) => socket.read(buf),
        };
        match read {
            Read::Data(count) if count == buf.len() => {}
            Read::Data(_) => self.ready.read = self.ready.closing,
            Read::Block => self.ready.read = false,
            Read::Eof | Read::Failed => {}
        }
        read
    }

    pub(crate) fn send(&mut self, (head, body): (&[u8], &[u8]), delivery: Delivery) -> Wrote {
        if !self.ready.write {
            return Wrote::Block;
        }
        let wrote = match &mut self.io {
            Io::Fd(fd) => sys::write(fd, head, body),
            Io::Ring(socket) => socket.write(head, body, delivery),
        };
        self.written(wrote, head.len() + body.len())
    }

    fn written(&mut self, wrote: Wrote, total: usize) -> Wrote {
        match wrote {
            Wrote::Bytes(count) if count == total => {}
            Wrote::Bytes(_) | Wrote::Block => self.ready.write = false,
            Wrote::Failed => {}
        }
        wrote
    }

    pub(crate) const fn lends(&self) -> bool {
        matches!(self.io, Io::Ring(_))
    }

    pub(crate) fn peek<R>(&mut self, view: impl FnOnce(&[u8]) -> R) -> Peek<R> {
        if !self.ready.read {
            return Peek::Block;
        }
        let Io::Ring(socket) = &self.io else {
            unreachable!("only a ring socket lends its received bytes")
        };
        let peeked = socket.peek(view);
        if matches!(peeked, Peek::Block) {
            self.ready.read = false;
        }
        peeked
    }

    pub(crate) fn forward(
        &mut self,
        source: &mut Self,
        (head, count): (&[u8], usize),
        delivery: Delivery,
    ) -> Wrote {
        if !self.ready.write {
            return Wrote::Block;
        }
        let (Io::Ring(sink), Io::Ring(from)) = (&self.io, &source.io) else {
            unreachable!("only ring sockets forward received bytes")
        };
        let (wrote, left) = sink.forward(from, (head, count), delivery);
        source.drained(left);
        self.written(wrote, head.len() + count)
    }

    pub(crate) fn skip(&mut self, count: usize) {
        let Io::Ring(socket) = &self.io else {
            unreachable!("only a ring socket lends its received bytes")
        };
        let left = socket.skip(count);
        self.drained(left);
    }

    fn drained(&mut self, left: usize) {
        if left == 0 {
            self.ready.read = self.ready.closing;
        }
    }

    pub(crate) fn shutdown_write(&self) {
        match &self.io {
            Io::Fd(fd) => sys::shutdown_write(fd),
            Io::Ring(socket) => socket.shutdown_write(),
        }
    }

    pub(crate) fn connect_succeeded(&self) -> bool {
        match &self.io {
            Io::Fd(fd) => sys::connect_result(fd),
            Io::Ring(socket) => socket.connected(),
        }
    }

    pub(crate) fn release(self) -> io::Result<Released> {
        match self.io {
            Io::Fd(fd) => Ok(Released {
                fd,
                unread: Vec::new(),
            }),
            Io::Ring(socket) => socket.release(),
        }
    }
}
