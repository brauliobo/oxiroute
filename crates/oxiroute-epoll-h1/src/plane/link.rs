use std::{io, os::fd::OwnedFd};

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

    pub(crate) fn send(&mut self, head: &[u8], body: &[u8]) -> Wrote {
        if !self.ready.write {
            return Wrote::Block;
        }
        let wrote = match &mut self.io {
            Io::Fd(fd) => sys::write(fd, head, body),
            Io::Ring(socket) => socket.write(head, body),
        };
        match wrote {
            Wrote::Bytes(count) if count == head.len() + body.len() => {}
            Wrote::Bytes(_) | Wrote::Block => self.ready.write = false,
            Wrote::Failed => {}
        }
        wrote
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

    pub(crate) fn release(self) -> io::Result<OwnedFd> {
        match self.io {
            Io::Fd(fd) => Ok(fd),
            Io::Ring(socket) => socket.release(),
        }
    }
}
