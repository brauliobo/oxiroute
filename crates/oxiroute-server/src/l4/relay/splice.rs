//! Bulk payload moved by the kernel: `splice(2)` from one socket into a pipe and from the pipe into the other
//! socket, so the bytes never enter user space.
//!
//! A direction starts here after it has read a full buffer, the point where the relay already treats it as bulk
//! traffic. Small messages cost more through a pipe than through a buffer (two extra system calls for nothing to
//! save), so they stay on the copy loop.

use std::{future::poll_fn, io, os::fd::OwnedFd};

use rustix::{
    io::Errno,
    pipe::{PipeFlags, SpliceFlags, fcntl_getpipe_size, pipe_with, splice},
};
use tokio::io::Interest;

use super::end::{RelayEnd, Shared};
use crate::l4::tcp::{
    DirectionFailure, RelayAccounting, RelayDirection, RelayOperation, bytes_to_u64,
};

/// Bytes moved per `splice`: a default pipe holds 16 pages.
pub(in crate::l4) const SPLICE_CHUNK: usize = 64 * 1024;

/// A pipe that carries the payload of one direction.
pub(in crate::l4) struct Pipe {
    read: OwnedFd,
    write: OwnedFd,
}

/// Whether a direction can hand its payload to the kernel now.
pub(in crate::l4) enum Start {
    Ready(Pipe),
    /// Bytes are still held in user space; the next full read asks again.
    Later,
    /// The ends or the kernel do not allow it for the rest of the connection.
    Never,
}

pub(in crate::l4) enum Finish {
    /// The source reached EOF after the given number of bytes went through the kernel.
    Eof(u64),
    /// The kernel refused the first `splice` on the source: nothing moved and the copy loop continues.
    Unsupported,
}

impl Pipe {
    /// A pipe of the default size, or `None` when the user's pipe quota is spent (the kernel then hands out a pipe of
    /// one page, which would turn every transfer into a page-sized one) or no descriptor is left.
    fn new() -> Option<Self> {
        let (read, write) = pipe_with(PipeFlags::NONBLOCK | PipeFlags::CLOEXEC).ok()?;
        (fcntl_getpipe_size(&write).ok()? >= SPLICE_CHUNK).then_some(Self { read, write })
    }
}

pub(in crate::l4) fn start<S: RelayEnd, T: RelayEnd>(
    source: &Shared<S>,
    sink: &Shared<T>,
) -> Start {
    if !(source.is_tcp() && sink.is_tcp()) {
        return Start::Never;
    }
    if !(source.read_side_is_direct() && sink.write_side_is_direct()) {
        return Start::Later;
    }
    Pipe::new().map_or(Start::Never, Start::Ready)
}

/// Moves `source` to `sink` through `pipe` until the source reaches EOF.
pub(in crate::l4) async fn run<S: RelayEnd, T: RelayEnd>(
    direction: RelayDirection,
    source: &Shared<S>,
    sink: &Shared<T>,
    pipe: &Pipe,
    accounting: &RelayAccounting<'_>,
) -> Result<Finish, DirectionFailure> {
    let mut total = 0_u64;
    loop {
        let moved = match fill(source, pipe).await {
            Ok(moved) => moved,
            Err(error) if total == 0 && is_unsupported(&error) => return Ok(Finish::Unsupported),
            Err(error) => {
                return Err(DirectionFailure::io(direction, RelayOperation::Read, error));
            }
        };
        if moved == 0 {
            return Ok(Finish::Eof(total));
        }
        accounting.read(direction, moved)?;
        let mut left = moved;
        while left > 0 {
            let written = drain(sink, pipe, left)
                .await
                .map_err(|source| DirectionFailure::io(direction, RelayOperation::Write, source))?;
            left -= written;
            accounting.written(direction, written)?;
        }
        total += bytes_to_u64(moved);
    }
}

/// `splice` fails with `EINVAL` on a descriptor that cannot be spliced and with `ENOSYS` when a sandbox filters it.
fn is_unsupported(error: &io::Error) -> bool {
    [Errno::INVAL, Errno::NOSYS]
        .into_iter()
        .any(|errno| error.raw_os_error() == Some(errno.raw_os_error()))
}

async fn fill<S: RelayEnd>(source: &Shared<S>, pipe: &Pipe) -> io::Result<usize> {
    poll_fn(|context| {
        source.poll_socket(context, Interest::READABLE, |socket| {
            Ok(splice(
                socket,
                None,
                &pipe.write,
                None,
                SPLICE_CHUNK,
                SpliceFlags::NONBLOCK,
            )?)
        })
    })
    .await
}

async fn drain<T: RelayEnd>(sink: &Shared<T>, pipe: &Pipe, length: usize) -> io::Result<usize> {
    let written = poll_fn(|context| {
        sink.poll_socket(context, Interest::WRITABLE, |socket| {
            Ok(splice(
                &pipe.read,
                None,
                socket,
                None,
                length,
                SpliceFlags::NONBLOCK,
            )?)
        })
    })
    .await?;
    if written == 0 {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "relay sink accepted no spliced bytes",
        ));
    }
    Ok(written)
}
