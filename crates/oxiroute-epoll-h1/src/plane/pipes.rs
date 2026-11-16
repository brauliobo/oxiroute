use std::{cell::Cell, io, os::fd::OwnedFd, rc::Rc};

use rustix::{
    io::Errno,
    net::{self, RecvFlags},
    pipe::{PipeFlags, SpliceFlags, fcntl_setpipe_size, pipe_with, splice},
};

use super::{
    config::Splice,
    sys::{Read, Wrote},
};

pub(crate) struct Pipe {
    read: OwnedFd,
    write: OwnedFd,
    capacity: usize,
    pending: usize,
    live: Rc<Cell<usize>>,
}

impl Pipe {
    fn open(bytes: usize, live: &Rc<Cell<usize>>) -> io::Result<Self> {
        let (read, write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
        let capacity = fcntl_setpipe_size(&write, bytes)?;
        live.set(live.get() + 1);
        Ok(Self {
            read,
            write,
            capacity,
            pending: 0,
            live: Rc::clone(live),
        })
    }

    pub(crate) const fn pending(&self) -> usize {
        self.pending
    }

    pub(crate) fn fill(&mut self, from: &OwnedFd, wanted: u64) -> Read {
        let len = usize::try_from(wanted).map_or(self.capacity, |wanted| wanted.min(self.capacity));
        loop {
            return match splice(from, None, &self.write, None, len, SpliceFlags::NONBLOCK) {
                Ok(0) => Read::Eof,
                Ok(count) => {
                    self.pending += count;
                    Read::Data(count)
                }
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => Read::Block,
                Err(_) => Read::Failed,
            };
        }
    }

    pub(crate) fn drain(&mut self, to: &OwnedFd) -> Wrote {
        loop {
            return match splice(
                &self.read,
                None,
                to,
                None,
                self.pending,
                SpliceFlags::NONBLOCK,
            ) {
                Ok(0) => Wrote::Failed,
                Ok(count) => {
                    self.pending -= count;
                    Wrote::Bytes(count)
                }
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => Wrote::Block,
                Err(_) => Wrote::Failed,
            };
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.live.set(self.live.get() - 1);
    }
}

pub(crate) struct Piped {
    pub(crate) pipe: Pipe,
    pub(crate) left: u64,
}

pub(crate) enum Carry {
    Undecided,
    Buffered,
    Piped(Piped),
}

pub(crate) struct Pipes {
    idle: Vec<Pipe>,
    live: Rc<Cell<usize>>,
    config: Splice,
}

impl Pipes {
    pub(crate) fn new(config: Splice) -> io::Result<Self> {
        let live = Rc::new(Cell::new(0));
        drop(Pipe::open(config.pipe_bytes, &live)?);
        Ok(Self {
            idle: Vec::with_capacity(config.pipes),
            live,
            config,
        })
    }

    pub(crate) const fn wants(&self, body: u64) -> bool {
        body >= self.config.threshold
    }

    pub(crate) fn lease(&mut self) -> Option<Pipe> {
        self.idle.pop().or_else(|| self.open())
    }

    fn open(&self) -> Option<Pipe> {
        if self.live.get() >= self.config.pipes {
            return None;
        }
        Pipe::open(self.config.pipe_bytes, &self.live).ok()
    }

    pub(crate) fn give_back(&mut self, pipe: Pipe) {
        if pipe.pending == 0 {
            self.idle.push(pipe);
        }
    }
}

pub(crate) fn unread(fd: &OwnedFd) -> Read {
    let mut byte = [0_u8; 1];
    loop {
        return match net::recv(fd, &mut byte[..], RecvFlags::PEEK | RecvFlags::DONTWAIT) {
            Ok((_, 0)) => Read::Eof,
            Ok(_) => Read::Data(1),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => Read::Block,
            Err(_) => Read::Failed,
        };
    }
}

#[cfg(test)]
mod tests {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

    use super::*;

    fn splice_config(pipes: usize) -> Splice {
        Splice {
            threshold: 4096,
            pipes,
            pipe_bytes: 64 * 1024,
        }
    }

    fn pair() -> (OwnedFd, OwnedFd) {
        socketpair(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )
        .expect("a socket pair")
    }

    #[test]
    fn the_pool_never_opens_more_pipes_than_its_bound_and_reuses_returned_ones() {
        let mut pipes = Pipes::new(splice_config(2)).expect("pipes");
        let first = pipes.lease().expect("first");
        let second = pipes.lease().expect("second");
        assert!(pipes.lease().is_none());
        pipes.give_back(first);
        let again = pipes.lease();
        assert!(again.is_some());
        assert_eq!(pipes.live.get(), 2);
        drop(second);
        assert_eq!(pipes.live.get(), 1);
    }

    #[test]
    fn a_pipe_that_is_dropped_frees_its_place_in_the_pool() {
        let mut pipes = Pipes::new(splice_config(1)).expect("pipes");
        drop(pipes.lease().expect("lease"));
        assert_eq!(pipes.live.get(), 0);
        assert!(pipes.lease().is_some());
    }

    #[test]
    fn a_pipe_that_still_holds_bytes_is_not_given_back_to_the_pool() {
        let mut pipes = Pipes::new(splice_config(1)).expect("pipes");
        let (source, sink) = pair();
        rustix::io::write(&sink, &[7_u8; 1000]).expect("write");
        let mut pipe = pipes.lease().expect("lease");
        assert_eq!(pipe.fill(&source, 1000), Read::Data(1000));
        assert_eq!(pipe.pending(), 1000);
        pipes.give_back(pipe);
        assert!(pipes.idle.is_empty());
        assert_eq!(pipes.live.get(), 0);
    }

    #[test]
    fn fill_takes_at_most_what_is_wanted_and_drain_moves_exactly_what_was_filled() {
        let mut pipes = Pipes::new(splice_config(1)).expect("pipes");
        let (source, sink) = pair();
        let (target, reader) = pair();
        rustix::io::write(&sink, &[1_u8; 3000]).expect("write");
        let mut pipe = pipes.lease().expect("lease");
        assert_eq!(pipe.fill(&source, 2000), Read::Data(2000));
        assert_eq!(pipe.drain(&target), Wrote::Bytes(2000));
        assert_eq!(pipe.pending(), 0);
        assert_eq!(pipe.fill(&source, 2000), Read::Data(1000));
        assert_eq!(pipe.fill(&source, 2000), Read::Block);
        assert_eq!(pipe.drain(&target), Wrote::Bytes(1000));
        let mut received = [0_u8; 4096];
        assert_eq!(rustix::io::read(&reader, &mut received), Ok(3000));
        pipes.give_back(pipe);
        assert_eq!(pipes.idle.len(), 1);
    }

    #[test]
    fn an_ended_source_reads_as_eof_and_an_empty_one_blocks() {
        let mut pipes = Pipes::new(splice_config(1)).expect("pipes");
        let (source, sink) = pair();
        let mut pipe = pipes.lease().expect("lease");
        assert_eq!(pipe.fill(&source, 10), Read::Block);
        drop(sink);
        assert_eq!(pipe.fill(&source, 10), Read::Eof);
    }

    #[test]
    fn a_pipe_size_the_kernel_refuses_fails_the_pool_at_startup() {
        let refused = Splice {
            pipe_bytes: usize::MAX / 2,
            ..splice_config(1)
        };
        assert!(Pipes::new(refused).is_err());
    }
}
