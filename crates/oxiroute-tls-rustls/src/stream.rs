use std::{
    cell::Cell,
    io::{self, BufRead, Read, Write},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::{Duration, Instant, SystemTime},
};

use async_trait::async_trait;
use pingora::protocols::{
    GetProxyDigest, GetSocketDigest, GetTimingDigest, Peek, Shutdown, SocketDigest, Ssl,
    TimingDigest, UniqueID, UniqueIDType,
    l4::stream::Stream as L4Stream,
    raw_connect::ProxyDigest,
    tls::{ALPN, SslDigest},
};
use rustls::ServerConnection;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    digest::{selected_alpn, ssl_digest},
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

/// An established server-side TLS connection.
#[derive(Debug)]
pub struct TlsStream {
    pump: Pump,
    digest: Arc<SslDigest>,
    alpn: Option<ALPN>,
    timing: TimingDigest,
    /// Plaintext not yet handed to rustls; the next flush, or the next write that does not fit, encrypts it.
    pending: Vec<u8>,
    sent: usize,
    close_sent: bool,
}

impl TlsStream {
    pub(crate) fn established(pump: Pump, handshake_started: Instant) -> Self {
        let digest = Arc::new(ssl_digest(&pump.conn));
        let alpn = selected_alpn(&pump.conn);
        let timing = TimingDigest {
            established_ts: SystemTime::now(),
            establishment_duration: Some(handshake_started.elapsed()),
            offload_wait_duration: None,
        };
        Self {
            pump,
            digest,
            alpn,
            timing,
            pending: Vec::new(),
            sent: 0,
            close_sent: false,
        }
    }

    /// The rustls connection, for what a handshake leaves to read (negotiated suite, resumption, peer certificates).
    #[must_use]
    pub const fn connection(&self) -> &ServerConnection {
        &self.pump.conn
    }

    /// Splits the stream into its transport and its rustls state, for what takes the connection over (kernel TLS needs
    /// the connection by value to extract its secrets). Plaintext a write still holds is lost: flush first.
    #[must_use]
    pub fn into_parts(self) -> (L4Stream, ServerConnection) {
        (self.pump.io, self.pump.conn)
    }

    /// Moves plaintext into rustls and its records into the transport's write buffer, without flushing the socket.
    fn poll_encrypt_pending(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.sent < self.pending.len() {
            let taken = self.pump.conn.writer().write(&self.pending[self.sent..])?;
            if taken == 0 {
                ready!(self.pump.poll_drain(context))?;
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
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let taken = loop {
            match self.pump.conn.writer().write(buffer)? {
                0 if !buffer.is_empty() => ready!(self.pump.poll_drain(context))?,
                taken => break taken,
            }
        };
        if let Poll::Ready(Err(error)) = self.pump.poll_drain(context) {
            return Poll::Ready(Err(error));
        }
        Poll::Ready(Ok(taken))
    }

    /// Moves as much of `buffers` as fits the record being filled into `pending`.
    fn poll_write_vectored_pending(
        &mut self,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.pending.len() >= COALESCE_VECTORED_LIMIT {
            ready!(self.poll_encrypt_pending(context))?;
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
}

impl AsyncRead for TlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            match this.pump.conn.reader().read(buffer.initialize_unfilled()) {
                Ok(count) => {
                    buffer.advance(count);
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
            if !ready!(this.pump.poll_pull(context))? {
                return Poll::Ready(Ok(()));
            }
            // What rustls answers on its own (key updates, alerts) leaves with the next write or flush at the latest.
            if let Poll::Ready(Err(error)) = this.pump.poll_drain(context) {
                return Poll::Ready(Err(error));
            }
        }
    }
}

impl AsyncWrite for TlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.pending.len() + buffer.len() > COALESCE_PLAIN_LIMIT {
            ready!(this.poll_encrypt_pending(context))?;
        }
        if buffer.len() <= COALESCE_PLAIN_LIMIT {
            this.pending.extend_from_slice(buffer);
            return Poll::Ready(Ok(buffer.len()));
        }
        this.poll_write_records(context, buffer)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.get_mut().poll_write_vectored_pending(context, buffers)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_encrypt_pending(context))?;
        ready!(this.pump.poll_drain(context))?;
        Pin::new(&mut this.pump.io).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_encrypt_pending(context))?;
        if !this.close_sent {
            this.pump.conn.send_close_notify();
            this.close_sent = true;
        }
        ready!(this.pump.poll_drain(context))?;
        Pin::new(&mut this.pump.io).poll_shutdown(context)
    }
}

impl UniqueID for TlsStream {
    fn id(&self) -> UniqueIDType {
        self.pump.io.id()
    }
}

impl Ssl for TlsStream {
    fn get_ssl_digest(&self) -> Option<Arc<SslDigest>> {
        Some(Arc::clone(&self.digest))
    }

    fn selected_alpn_proto(&self) -> Option<ALPN> {
        self.alpn.clone()
    }
}

impl GetTimingDigest for TlsStream {
    fn get_timing_digest(&self) -> Vec<Option<TimingDigest>> {
        let mut digests = self.pump.io.get_timing_digest();
        digests.push(Some(self.timing.clone()));
        digests
    }

    fn get_read_pending_time(&self) -> Duration {
        self.pump.io.get_read_pending_time()
    }

    fn get_write_pending_time(&self) -> Duration {
        self.pump.io.get_write_pending_time()
    }
}

impl GetProxyDigest for TlsStream {
    fn get_proxy_digest(&self) -> Option<Arc<ProxyDigest>> {
        self.pump.io.get_proxy_digest()
    }
}

impl GetSocketDigest for TlsStream {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        self.pump.io.get_socket_digest()
    }

    fn set_socket_digest(&mut self, socket_digest: SocketDigest) {
        self.pump.io.set_socket_digest(socket_digest);
    }
}

impl Peek for TlsStream {
    /// Ready while decrypted bytes wait or the connection has ended; otherwise the transport decides, so a connection
    /// with nothing to read is not decrypted again to find that out.
    fn poll_read_ready(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.pump.conn.reader().fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.pump.io.poll_read_ready(context)
            }
            _ => Poll::Ready(Ok(())),
        }
    }
}

#[async_trait]
impl Shutdown for TlsStream {
    async fn shutdown(&mut self) {
        if let Err(error) = AsyncWriteExt::shutdown(self).await {
            log::warn!("TLS shutdown failed, {error}");
        }
    }
}
