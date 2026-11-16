use std::os::fd::OwnedFd;

use rustix::event::epoll::EventFlags;

use super::sys::{self, Read, Wrote};

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

pub(crate) struct Link {
    pub(crate) fd: OwnedFd,
    pub(crate) ready: Ready,
    pub(crate) interest: (bool, bool),
}

impl Link {
    pub(crate) const fn new(fd: OwnedFd) -> Self {
        Self {
            fd,
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
        let read = sys::read(&self.fd, buf);
        match read {
            Read::Data(count) if count == buf.len() => {}
            Read::Data(_) => self.ready.read = self.ready.closing,
            Read::Block => self.ready.read = false,
            Read::Eof | Read::Failed => {}
        }
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
        if !self.ready.write {
            return Wrote::Block;
        }
        let wrote = sys::write(&self.fd, head, body);
        match wrote {
            Wrote::Bytes(count) if count == head.len() + body.len() => {}
            Wrote::Bytes(_) | Wrote::Block => self.ready.write = false,
            Wrote::Failed => {}
        }
        wrote
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
