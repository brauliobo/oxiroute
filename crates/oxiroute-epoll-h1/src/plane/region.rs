use std::ops::{Deref, DerefMut};

use super::{link::Link, sys::Read};

pub(crate) struct Region {
    bytes: Vec<u8>,
    limit: usize,
    tail: usize,
}

impl Region {
    pub(crate) fn new(initial: usize, limit: usize, tail: usize) -> Self {
        Self {
            bytes: vec![0; initial.min(limit)],
            limit,
            tail,
        }
    }

    pub(crate) const fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) const fn usable(&self) -> usize {
        self.bytes.len() - self.tail
    }

    pub(crate) const fn exhausted(&self, used: usize) -> bool {
        used >= self.usable() && self.bytes.len() == self.limit
    }

    fn grow(&mut self) {
        let size = (self.bytes.len() * 2).min(self.limit);
        self.bytes.resize(size, 0);
    }

    pub(crate) fn recv(&mut self, link: &mut Link, start: usize, room: usize) -> Read {
        if start >= self.usable() {
            self.grow();
        }
        let end = start.saturating_add(room).min(self.usable());
        let read = link.recv(&mut self.bytes[start..end]);
        if matches!(read, Read::Data(count) if start + count == self.usable()) {
            self.grow();
        }
        read
    }
}

impl Deref for Region {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl DerefMut for Region {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
        os::fd::OwnedFd,
    };

    use super::*;

    fn pair() -> (TcpStream, Link) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let sender = TcpStream::connect(listener.local_addr().expect("address")).expect("connect");
        let (receiver, _) = listener.accept().expect("accept");
        receiver.set_nonblocking(true).expect("nonblocking");
        let mut link = Link::new(crate::plane::link::Io::Fd(OwnedFd::from(receiver)));
        link.ready.read = true;
        (sender, link)
    }

    #[test]
    fn a_region_starts_small_and_doubles_each_time_a_read_fills_it() {
        let (mut sender, mut link) = pair();
        let mut region = Region::new(8, 32, 2);
        sender.write_all(&[1; 64]).expect("send");
        let mut sizes = Vec::new();
        for _ in 0..4 {
            region.recv(&mut link, 0, usize::MAX);
            sizes.push(region.len());
        }
        assert_eq!(sizes, [16, 32, 32, 32]);
        assert_eq!(region.usable(), 30);
    }

    #[test]
    fn a_region_with_no_room_left_grows_before_it_reads_and_reports_when_it_cannot() {
        let (mut sender, mut link) = pair();
        let mut region = Region::new(8, 16, 0);
        sender.write_all(&[7; 12]).expect("send");
        assert_eq!(region.recv(&mut link, 8, usize::MAX), Read::Data(8));
        assert_eq!(region.len(), 16);
        assert!(region.exhausted(16));
        assert!(!region.exhausted(15));
    }

    #[test]
    fn a_read_bounded_by_the_caller_does_not_make_the_region_grow() {
        let (mut sender, mut link) = pair();
        let mut region = Region::new(8, 64, 0);
        sender.write_all(&[3; 64]).expect("send");
        assert_eq!(region.recv(&mut link, 0, 4), Read::Data(4));
        assert_eq!(region.len(), 8);
    }
}
