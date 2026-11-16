use std::{
    io::{self, IoSlice},
    os::fd::OwnedFd,
};

use super::super::{
    link::Ready,
    sys::{self, Read, Wrote},
};

pub(crate) struct Transport<'a> {
    fd: &'a OwnedFd,
    ready: &'a mut Ready,
}

impl<'a> Transport<'a> {
    pub(crate) const fn new(fd: &'a OwnedFd, ready: &'a mut Ready) -> Self {
        Self { fd, ready }
    }
}

impl io::Read for Transport<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.ready.read {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let read = sys::read(self.fd, buf);
        self.ready.after_read(read, buf.len());
        match read {
            Read::Data(count) => Ok(count),
            Read::Eof => Ok(0),
            Read::Block => Err(io::ErrorKind::WouldBlock.into()),
            Read::Failed => Err(io::ErrorKind::ConnectionReset.into()),
        }
    }
}

impl io::Write for Transport<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_vectored(&[IoSlice::new(buf)])
    }

    fn write_vectored(&mut self, slices: &[IoSlice<'_>]) -> io::Result<usize> {
        if !self.ready.write {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let wrote = sys::write_vectored(self.fd, slices);
        self.ready
            .after_write(wrote, slices.iter().map(|slice| slice.len()).sum());
        match wrote {
            Wrote::Bytes(count) => Ok(count),
            Wrote::Block => Err(io::ErrorKind::WouldBlock.into()),
            Wrote::Failed => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
