//! The two ends of a TCP relay and the shared access that lets one direction hand its payload to the kernel.

use std::{
    io,
    pin::Pin,
    sync::{Mutex, MutexGuard},
    task::{Context, Poll, ready},
};

use pingora::protocols::{IO, Stream, l4::stream::Stream as L4Stream};
use tokio::{
    io::{AsyncRead, AsyncWrite, Interest, ReadBuf},
    net::TcpStream,
};

use crate::l4::proxy_protocol::PrefixedStream;

/// One end of a relay.
///
/// An end that is a plain TCP socket can have its payload moved by the kernel (`splice(2)`) without entering user
/// space. Every other end (TLS, Unix sockets, test doubles) is copied through the relay's buffers.
pub trait RelayEnd: AsyncRead + AsyncWrite + Unpin {
    /// The TCP socket behind this end when the bytes on it are the payload itself: no TLS and no other framing.
    fn tcp_socket(&self) -> Option<&TcpStream> {
        None
    }

    /// Whether bytes already read from the socket wait in user space for the reader.
    fn has_read_ahead(&self) -> bool {
        false
    }

    /// Whether bytes accepted from the writer wait in user space to be sent.
    fn has_write_behind(&self) -> bool {
        false
    }
}

impl RelayEnd for TcpStream {
    fn tcp_socket(&self) -> Option<&TcpStream> {
        Some(self)
    }
}

impl RelayEnd for L4Stream {
    fn tcp_socket(&self) -> Option<&TcpStream> {
        L4Stream::tcp_socket(self)
    }

    fn has_read_ahead(&self) -> bool {
        L4Stream::has_read_ahead(self)
    }

    fn has_write_behind(&self) -> bool {
        L4Stream::has_write_behind(self)
    }
}

/// A boxed Pingora stream is a plain end when the stream under the box is the L4 stream (no TLS layer).
impl RelayEnd for Stream {
    fn tcp_socket(&self) -> Option<&TcpStream> {
        l4(self)?.tcp_socket()
    }

    fn has_read_ahead(&self) -> bool {
        l4(self).is_some_and(L4Stream::has_read_ahead)
    }

    fn has_write_behind(&self) -> bool {
        l4(self).is_some_and(L4Stream::has_write_behind)
    }
}

fn l4(stream: &Stream) -> Option<&L4Stream> {
    IO::as_any(&**stream).downcast_ref()
}

impl<S: RelayEnd> RelayEnd for PrefixedStream<S> {
    fn tcp_socket(&self) -> Option<&TcpStream> {
        self.inner().tcp_socket()
    }

    fn has_read_ahead(&self) -> bool {
        self.has_prefix() || self.inner().has_read_ahead()
    }

    fn has_write_behind(&self) -> bool {
        self.inner().has_write_behind()
    }
}

/// An end that is always copied through the relay's buffers.
pub struct CopyOnly<T>(pub T);

impl<T: AsyncRead + Unpin> AsyncRead for CopyOnly<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(context, buffer)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for CopyOnly<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(context)
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> RelayEnd for CopyOnly<T> {}

/// An end shared by the two directions of a relay: one reads it and the other writes it.
///
/// This is `tokio::io::split` with access to the end, which a direction needs to wait for readiness of the socket
/// itself and to move payload with `splice(2)`. The lock is held for the length of one poll and never across an
/// await point, and both directions run in the same task, so it is never contended.
pub(in crate::l4) struct Shared<T>(Mutex<T>);

impl<T: RelayEnd> Shared<T> {
    pub(in crate::l4) const fn new(end: T) -> Self {
        Self(Mutex::new(end))
    }

    pub(in crate::l4) const fn handle(&self) -> Handle<'_, T> {
        Handle(&self.0)
    }

    fn end(&self) -> MutexGuard<'_, T> {
        self.0
            .lock()
            .expect("a relay end is not held across a panic")
    }

    /// Whether the end is a TCP socket whose payload can be moved without user-space processing.
    pub(in crate::l4) fn is_tcp(&self) -> bool {
        self.end().tcp_socket().is_some()
    }

    /// Whether no byte of the end is held in user space on the read side.
    pub(in crate::l4) fn read_side_is_direct(&self) -> bool {
        !self.end().has_read_ahead()
    }

    /// Whether no byte of the end is held in user space on the write side.
    pub(in crate::l4) fn write_side_is_direct(&self) -> bool {
        !self.end().has_write_behind()
    }

    /// Runs `operation` on the socket once it is ready for `interest`, waiting for readiness again whenever the
    /// operation reports `WouldBlock`.
    pub(in crate::l4) fn poll_socket<R>(
        &self,
        context: &mut Context<'_>,
        interest: Interest,
        mut operation: impl FnMut(&TcpStream) -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        let end = self.end();
        let socket = end
            .tcp_socket()
            .expect("splice only starts on a TCP socket");
        loop {
            if interest == Interest::READABLE {
                ready!(socket.poll_read_ready(context))?;
            } else {
                ready!(socket.poll_write_ready(context))?;
            }
            match socket.try_io(interest, || operation(socket)) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return Poll::Ready(result),
            }
        }
    }
}

pub(in crate::l4) struct Handle<'a, T>(&'a Mutex<T>);

impl<T: RelayEnd> Handle<'_, T> {
    fn end(&self) -> MutexGuard<'_, T> {
        self.0
            .lock()
            .expect("a relay end is not held across a panic")
    }
}

impl<T: RelayEnd> AsyncRead for Handle<'_, T> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.end()).poll_read(context, buffer)
    }
}

impl<T: RelayEnd> AsyncWrite for Handle<'_, T> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.end()).poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.end()).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.end()).poll_shutdown(context)
    }
}
