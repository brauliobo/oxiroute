use std::os::fd::OwnedFd;

use rustix::event::epoll::EventFlags;
use rustls::ServerConnection;

use super::{
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

pub(crate) struct Link {
    pub(crate) fd: OwnedFd,
    pub(crate) ready: Ready,
    pub(crate) interest: (bool, bool),
    tls: Option<Box<Session>>,
    shut: bool,
}

impl Link {
    pub(crate) const fn new(fd: OwnedFd) -> Self {
        Self::with(fd, None)
    }

    pub(crate) fn secure(fd: OwnedFd, session: Session) -> Self {
        Self::with(fd, Some(Box::new(session)))
    }

    const fn with(fd: OwnedFd, tls: Option<Box<Session>>) -> Self {
        Self {
            fd,
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
        let Self { fd, ready, tls, .. } = self;
        tls.as_deref_mut()
            .map(|tls| (tls, Transport::new(fd, ready)))
    }

    pub(crate) fn recv(&mut self, buf: &mut [u8]) -> Read {
        if let Some((tls, mut io)) = self.secured() {
            return tls.recv(&mut io, buf);
        }
        if !self.ready.read {
            return Read::Block;
        }
        let read = sys::read(&self.fd, buf);
        self.ready.after_read(read, buf.len());
        read
    }

    pub(crate) fn has_input(&mut self) -> bool {
        if !self.ready.read {
            return false;
        }
        let pending = !matches!(sys::peek(&self.fd), Read::Block);
        self.ready.read = pending;
        pending
    }

    pub(crate) fn unsolicited(&mut self) -> bool {
        self.ready.hangup || self.has_input()
    }

    pub(crate) fn send(&mut self, head: &[u8], body: &[u8]) -> Wrote {
        if let Some((tls, mut io)) = self.secured() {
            return tls.send(&mut io, head, body);
        }
        if !self.ready.write {
            return Wrote::Block;
        }
        let wrote = sys::write(&self.fd, head, body);
        self.ready.after_write(wrote, head.len() + body.len());
        wrote
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
            sys::shutdown_write(&self.fd);
        }
        finish
    }

    pub(crate) fn farewell(&mut self) {
        if let Some((tls, mut io)) = self.secured() {
            tls.finish(&mut io);
        }
    }

    pub(crate) fn into_parts(self) -> (OwnedFd, Option<Box<ServerConnection>>) {
        (self.fd, self.tls.and_then(|tls| tls.into_connection()))
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
        let mut link = Link::new(near);
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
        assert_eq!(sys::read(&link.fd, &mut byte), Read::Data(1));
    }

    #[test]
    fn an_ended_peer_is_input() {
        let (mut link, far) = linked();
        drop(far);
        assert!(link.unsolicited());
    }
}
