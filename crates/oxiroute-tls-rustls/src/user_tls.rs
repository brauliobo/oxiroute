use std::{
    cell::Cell,
    io::{self, BufRead, Write},
    pin::Pin,
    task::{Context, Poll, ready},
};

use pingora::protocols::{Peek, l4::stream::Stream as L4Stream};
use rustls::ServerConnection;
use tokio::{
    io::{AsyncWrite, ReadBuf},
    net::TcpStream,
};

use crate::{
    kernel::socket::{Failure, KernelIo},
    kernel::{self, KernelTls, Suite},
    pump::Pump,
};

/// Plaintext held back for the next record when a plain write is small: the size of the accepted socket's own write
/// buffer, so data that used to wait for a flush still waits for one and data that used to leave at once still does.
const COALESCE_PLAIN_LIMIT: usize = 1460;
/// Vectored writes (HTTP/2 frames, chunked bodies) may leave pending up to a whole TLS record: their writers flush
/// right after, and their bytes are packed into full records.
const COALESCE_VECTORED_LIMIT: usize = 16 * 1024;
/// A drained buffer larger than this goes back to the thread's spare instead of staying with the connection.
const COALESCE_RETAINED_CAPACITY: usize = 2048;

thread_local! {
    /// The record-sized buffer of the connections of this thread that are writing a body: a connection that went idle
    /// keeps none.
    static SPARE: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

/// A connection whose records rustls encrypts and decrypts in user space.
#[derive(Debug)]
pub(crate) struct UserTls {
    pump: Pump,
    kernel: Option<KernelTls>,
    /// Plaintext not yet handed to rustls; the next flush, or the next write that does not fit, encrypts it.
    pending: Vec<u8>,
    sent: usize,
    close_sent: bool,
}

impl UserTls {
    pub(crate) const fn new(pump: Pump, kernel: Option<KernelTls>) -> Self {
        Self {
            pump,
            kernel,
            pending: Vec::new(),
            sent: 0,
            close_sent: false,
        }
    }

    pub(crate) const fn connection(&self) -> &ServerConnection {
        &self.pump.conn
    }

    /// The suite the kernel would run this connection with, when it is offered and nothing keeps the connection in
    /// user space right now: the socket is TCP, the peer has not sent a record that is half read or plaintext that
    /// is unread, and nothing has been read ahead of rustls.
    pub(crate) fn kernel_suite(&self, io: &L4Stream) -> Option<Suite> {
        let offered = self.kernel?;
        let suite = Suite::of(&self.pump.conn).filter(|suite| offered.offers(*suite))?;
        let ready = io.tcp_socket().is_some()
            && !self.close_sent
            && self.pump.has_nothing_buffered()
            && !io.has_read_ahead();
        ready.then_some(suite)
    }

    pub(crate) const fn keep_in_user_space(&mut self) {
        self.kernel = None;
    }

    /// Takes the connection over for the kernel: the secrets leave rustls, so a failure ends the connection.
    pub(crate) fn into_kernel(self, socket: &TcpStream, suite: Suite) -> Result<KernelIo, Failure> {
        let secrets = self
            .pump
            .conn
            .dangerous_extract_secrets()
            .map_err(io::Error::other)?;
        kernel::install(socket, suite, &secrets)?;
        Ok(KernelIo::default())
    }

    /// Moves plaintext into rustls and its records into the transport's write buffer, without flushing the socket.
    fn poll_encrypt_pending(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        while self.sent < self.pending.len() {
            let taken = self.pump.conn.writer().write(&self.pending[self.sent..])?;
            if taken == 0 {
                ready!(self.pump.poll_drain(io, context))?;
            }
            self.sent += taken;
        }
        self.sent = 0;
        self.pending.clear();
        if self.pending.capacity() > COALESCE_RETAINED_CAPACITY {
            SPARE.set(std::mem::take(&mut self.pending));
        }
        Poll::Ready(Ok(()))
    }

    /// Encrypts `buffer` as records of its own. Whatever the socket does not take yet stays queued in rustls until the
    /// next write or flush.
    fn poll_write_records(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let taken = loop {
            match self.pump.conn.writer().write(buffer)? {
                0 if !buffer.is_empty() => ready!(self.pump.poll_drain(io, context))?,
                taken => break taken,
            }
        };
        if let Poll::Ready(Err(error)) = self.pump.poll_drain(io, context) {
            return Poll::Ready(Err(error));
        }
        Poll::Ready(Ok(taken))
    }

    pub(crate) fn poll_read(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            // Copies only the plaintext that is there: reading into `initialize_unfilled` would zero the caller's whole
            // spare capacity (a body buffer is 16 to 64 KiB) on every read and make it resident.
            let mut reader = self.pump.conn.reader();
            match reader.fill_buf() {
                Ok(plaintext) => {
                    let count = plaintext.len().min(buffer.remaining());
                    buffer.put_slice(&plaintext[..count]);
                    reader.consume(count);
                    return Poll::Ready(Ok(()));
                }
                // The peer closed without a close_notify: the stream ends the way a closed connection does.
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    return Poll::Ready(Ok(()));
                }
                Err(error) if error.kind() != io::ErrorKind::WouldBlock => {
                    return Poll::Ready(Err(error));
                }
                Err(_) => {}
            }
            if !ready!(self.pump.poll_pull(io, context))? {
                return Poll::Ready(Ok(()));
            }
            // What rustls answers on its own (key updates, alerts) leaves with the next write or flush at the latest.
            if let Poll::Ready(Err(error)) = self.pump.poll_drain(io, context) {
                return Poll::Ready(Err(error));
            }
        }
    }

    pub(crate) fn poll_write(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.pending.len() + buffer.len() > COALESCE_PLAIN_LIMIT {
            ready!(self.poll_encrypt_pending(io, context))?;
        }
        if buffer.len() <= COALESCE_PLAIN_LIMIT {
            self.pending.extend_from_slice(buffer);
            return Poll::Ready(Ok(buffer.len()));
        }
        self.poll_write_records(io, context, buffer)
    }

    /// Moves as much of `buffers` as fits the record being filled into `pending`.
    pub(crate) fn poll_write_vectored(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.pending.len() >= COALESCE_VECTORED_LIMIT {
            ready!(self.poll_encrypt_pending(io, context))?;
        }
        if self.pending.capacity() == 0 {
            self.pending = SPARE.take();
        }
        let room = COALESCE_VECTORED_LIMIT - self.pending.len();
        self.pending.reserve(room);
        let mut taken = 0;
        for buffer in buffers {
            let part = &buffer[..buffer.len().min(room - taken)];
            self.pending.extend_from_slice(part);
            taken += part.len();
        }
        Poll::Ready(Ok(taken))
    }

    pub(crate) fn poll_flush(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        ready!(self.poll_encrypt_pending(io, context))?;
        ready!(self.pump.poll_drain(io, context))?;
        Pin::new(io).poll_flush(context)
    }

    pub(crate) fn poll_shutdown(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        ready!(self.poll_encrypt_pending(io, context))?;
        if !self.close_sent {
            self.pump.conn.send_close_notify();
            self.close_sent = true;
        }
        ready!(self.pump.poll_drain(io, context))?;
        Pin::new(io).poll_shutdown(context)
    }

    /// Ready while decrypted bytes wait or the connection has ended; otherwise the transport decides, so a connection
    /// with nothing to read is not decrypted again to find that out.
    pub(crate) fn poll_read_ready(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        match self.pump.conn.reader().fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => io.poll_read_ready(context),
            _ => Poll::Ready(Ok(())),
        }
    }
}
