use std::{io, os::fd::OwnedFd};

use oxiroute_uring::{Peek, Released};
use rustix::event::epoll::EventFlags;
use rustls::ServerConnection;

use super::{
    driver::RingSocket,
    service::Secured,
    sys::{self, Read, Wrote},
    tls::{Finish, Progress, Session, Transport},
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

    pub(crate) fn after_read(&mut self, read: Read, room: usize) {
        match read {
            Read::Data(count) if count == room => {}
            Read::Data(_) => self.read = self.closing,
            Read::Block => self.read = false,
            Read::Eof | Read::Failed => {}
        }
    }

    pub(crate) fn after_write(&mut self, wrote: Wrote, total: usize) {
        match wrote {
            Wrote::Bytes(count) if count == total => {}
            Wrote::Bytes(_) | Wrote::Block => self.write = false,
            Wrote::Failed => {}
        }
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
    tls: Option<Box<Session>>,
    shut: bool,
}

impl Link {
    pub(crate) const fn new(io: Io) -> Self {
        Self::with(io, None)
    }

    pub(crate) fn secure(fd: OwnedFd, session: Session) -> Self {
        Self::with(Io::Fd(fd), Some(Box::new(session)))
    }

    const fn with(io: Io, tls: Option<Box<Session>>) -> Self {
        Self {
            io,
            ready: Ready {
                read: false,
                write: false,
                hangup: false,
                closing: false,
            },
            interest: (true, true),
            tls,
            shut: false,
        }
    }

    pub(crate) const fn encrypted(&self) -> bool {
        self.tls.is_some()
    }

    pub(crate) fn can_read(&self) -> bool {
        self.ready.read || self.tls.as_ref().is_some_and(|tls| tls.has_plaintext())
    }

    fn secured(&mut self) -> Option<(&mut Session, Transport<'_>)> {
        let Self {
            io: Io::Fd(fd),
            ready,
            tls: Some(tls),
            ..
        } = self
        else {
            return None;
        };
        Some((&mut **tls, Transport::new(fd, ready)))
    }

    pub(crate) fn fd(&self) -> &OwnedFd {
        match &self.io {
            Io::Fd(fd) => fd,
            Io::Ring(_) => unreachable!("only a link on a raw descriptor is spliced"),
        }
    }

    pub(crate) fn recv(&mut self, buf: &mut [u8]) -> Read {
        if let Some((tls, mut io)) = self.secured() {
            return tls.recv(&mut io, buf);
        }
        if !self.ready.read {
            return Read::Block;
        }
        let read = match &self.io {
            Io::Fd(fd) => sys::read(fd, buf),
            Io::Ring(socket) => socket.read(buf),
        };
        self.ready.after_read(read, buf.len());
        read
    }

    pub(crate) fn has_input(&mut self) -> bool {
        if !self.ready.read {
            return false;
        }
        let pending = match &self.io {
            Io::Fd(fd) => !matches!(sys::peek(fd), Read::Block),
            Io::Ring(socket) => !matches!(socket.peek(|_| ()), Peek::Block),
        };
        self.ready.read = pending;
        pending
    }

    pub(crate) fn unsolicited(&mut self) -> bool {
        self.ready.hangup || self.has_input()
    }

    pub(crate) fn send(&mut self, (head, body): (&[u8], &[u8]), delivery: Delivery) -> Wrote {
        if let Some((tls, mut io)) = self.secured() {
            return tls.send(&mut io, head, body);
        }
        if !self.ready.write {
            return Wrote::Block;
        }
        let wrote = match &self.io {
            Io::Fd(fd) => sys::write(fd, head, body),
            Io::Ring(socket) => socket.write(head, body, delivery),
        };
        self.ready.after_write(wrote, head.len() + body.len());
        wrote
    }

    pub(crate) fn facts(&self) -> Option<Secured> {
        self.tls.as_ref().map(|tls| tls.facts())
    }

    pub(crate) fn handshake(&mut self) -> Progress {
        let (tls, mut io) = self
            .secured()
            .expect("only an encrypted link has a handshake");
        tls.advance(&mut io)
    }

    pub(crate) fn flush(&mut self) -> bool {
        self.secured()
            .is_none_or(|(tls, mut io)| tls.flush(&mut io))
    }

    pub(crate) fn finish(&mut self) -> Finish {
        let finish = self
            .secured()
            .map_or(Finish::Done, |(tls, mut io)| tls.finish(&mut io));
        if finish == Finish::Done && !self.shut {
            self.shut = true;
            self.shutdown_write();
        }
        finish
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
        self.ready.after_write(wrote, head.len() + count);
        wrote
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

    pub(crate) fn farewell(&mut self) {
        if let Some((tls, mut io)) = self.secured() {
            tls.finish(&mut io);
        }
    }

    pub(crate) fn into_parts(self) -> io::Result<(Released, Option<Box<ServerConnection>>)> {
        let tls = self.tls.and_then(|tls| tls.into_connection());
        let released = match self.io {
            Io::Fd(fd) => Released {
                fd,
                unread: Vec::new(),
            },
            Io::Ring(socket) => socket.release()?,
        };
        Ok((released, tls))
    }
}

#[cfg(test)]
mod tests {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

    use super::*;

    fn linked() -> (Link, OwnedFd) {
        let (near, far) = socketpair(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )
        .expect("a socket pair");
        let mut link = Link::new(Io::Fd(near));
        link.ready.read = true;
        (link, far)
    }

    #[test]
    fn a_readiness_left_over_from_consumed_data_is_not_input() {
        let (mut link, _far) = linked();
        assert!(!link.has_input());
        assert!(!link.ready.read);
        assert!(!link.unsolicited());
    }

    #[test]
    fn unread_bytes_are_input_and_stay_unread() {
        let (mut link, far) = linked();
        rustix::io::write(&far, b"x").expect("write");
        assert!(link.has_input());
        assert!(link.ready.read);
        let mut byte = [0_u8; 1];
        assert_eq!(sys::read(link.fd(), &mut byte), Read::Data(1));
    }

    #[test]
    fn an_ended_peer_is_input() {
        let (mut link, far) = linked();
        drop(far);
        assert!(link.unsolicited());
    }
}
