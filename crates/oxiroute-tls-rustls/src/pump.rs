use std::{
    io::{self, Read, Write},
    pin::Pin,
    task::{Context, Poll},
};

use pingora::protocols::l4::stream::Stream as L4Stream;
use rustls::ServerConnection;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Lends the transport to rustls' synchronous I/O: a transport that is not ready reports `WouldBlock` and has
/// registered the waker of the task that holds `context`.
pub(crate) struct Transport<'a, 'b> {
    pub(crate) io: &'a mut L4Stream,
    pub(crate) context: &'a mut Context<'b>,
}

impl Read for Transport<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut buffer = ReadBuf::new(buffer);
        match Pin::new(&mut *self.io).poll_read(self.context, &mut buffer) {
            Poll::Ready(Ok(())) => Ok(buffer.filled().len()),
            Poll::Ready(Err(error)) => Err(error),
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

impl Write for Transport<'_, '_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match Pin::new(&mut *self.io).poll_write(self.context, buffer) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    fn write_vectored(&mut self, buffers: &[io::IoSlice<'_>]) -> io::Result<usize> {
        match Pin::new(&mut *self.io).poll_write_vectored(self.context, buffers) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match Pin::new(&mut *self.io).poll_flush(self.context) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

/// The transport and the rustls state machine of one connection: ciphertext moves between them here.
#[derive(Debug)]
pub(crate) struct Pump {
    pub(crate) io: L4Stream,
    pub(crate) conn: ServerConnection,
}

impl Pump {
    pub(crate) const fn new(io: L4Stream, conn: ServerConnection) -> Self {
        Self { io, conn }
    }

    /// Reads ciphertext and lets rustls process it. `Ok(false)` is the end of the transport.
    pub(crate) fn poll_pull(&mut self, context: &mut Context<'_>) -> Poll<io::Result<bool>> {
        let read = self.conn.read_tls(&mut Transport {
            io: &mut self.io,
            context,
        });
        match read {
            Ok(0) => Poll::Ready(Ok(false)),
            Ok(_) => Poll::Ready(self.process().map(|()| true)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn process(&mut self) -> io::Result<()> {
        self.conn.process_new_packets().map(drop).map_err(|error| {
            self.send_alert();
            io::Error::new(io::ErrorKind::InvalidData, error)
        })
    }

    /// Best effort: the connection is failing already and the peer only gains from learning why.
    fn send_alert(&mut self) {
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let _ = self.poll_drain(&mut context);
        let _ = Pin::new(&mut self.io).poll_flush(&mut context);
    }

    /// Hands the ciphertext rustls holds to the transport's write buffer.
    pub(crate) fn poll_drain(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.conn.wants_write() {
            let written = self.conn.write_tls(&mut Transport {
                io: &mut self.io,
                context,
            });
            match written {
                Ok(0) => return Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Poll::Pending,
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
        Poll::Ready(Ok(()))
    }

    /// Runs the handshake to its end: the server flight and the tickets leave, the client's messages arrive.
    pub(crate) fn poll_handshake(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            std::task::ready!(self.poll_drain(context))?;
            std::task::ready!(Pin::new(&mut self.io).poll_flush(context))?;
            if !self.conn.is_handshaking() {
                return Poll::Ready(Ok(()));
            }
            if !std::task::ready!(self.poll_pull(context))? {
                return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
            }
        }
    }
}
