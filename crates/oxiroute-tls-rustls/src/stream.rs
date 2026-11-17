use std::{
    io,
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
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
};

use crate::{
    digest::{selected_alpn, ssl_digest},
    kernel::socket::{Failure, KernelIo},
    kernel::{self, Offer},
    pump::Pump,
    records::Records,
    user_tls::UserTls,
};

/// Who encrypts the records of a connection.
#[derive(Debug)]
enum Tls {
    User(Box<UserTls>),
    Kernel(Box<KernelIo>),
    Failed(Failure),
}

/// An established server-side TLS connection.
#[derive(Debug)]
pub struct TlsStream {
    io: L4Stream,
    tls: Tls,
    digest: Arc<SslDigest>,
    alpn: Option<ALPN>,
    timing: TimingDigest,
    /// Plaintext another reader took from this connection before it was handed over: it is read again first.
    replay: Vec<u8>,
    replayed: usize,
}

impl TlsStream {
    pub(crate) fn established(
        io: L4Stream,
        pump: Pump,
        offer: Option<Offer>,
        handshake_started: Instant,
    ) -> Self {
        let digest = Arc::new(ssl_digest(&pump.conn));
        let alpn = selected_alpn(&pump.conn);
        let timing = TimingDigest {
            established_ts: SystemTime::now(),
            establishment_duration: Some(handshake_started.elapsed()),
            offload_wait_duration: None,
        };
        Self {
            io,
            tls: Tls::User(Box::new(UserTls::new(pump, offer))),
            digest,
            alpn,
            timing,
            replay: Vec::new(),
            replayed: 0,
        }
    }

    /// Continues a connection whose handshake another acceptor completed: `replay` is the plaintext that acceptor
    /// already read from it and the next reads return it first.
    #[must_use]
    pub fn resume(io: L4Stream, conn: ServerConnection, replay: Vec<u8>) -> Self {
        let mut stream = Self::established(
            io,
            Pump::new(conn, Records::default()),
            None,
            Instant::now(),
        );
        stream.replay = replay;
        stream
    }

    fn read_replay(&mut self, buffer: &mut ReadBuf<'_>) -> bool {
        let pending = &self.replay[self.replayed..];
        if pending.is_empty() {
            return false;
        }
        let count = pending.len().min(buffer.remaining());
        buffer.put_slice(&pending[..count]);
        self.replayed += count;
        if self.replayed == self.replay.len() {
            self.replay = Vec::new();
            self.replayed = 0;
        }
        true
    }

    /// The rustls connection, for what a handshake leaves to read (negotiated suite, resumption, peer certificates).
    /// A connection the kernel took over has none left.
    #[must_use]
    pub const fn connection(&self) -> Option<&ServerConnection> {
        match &self.tls {
            Tls::User(user) => Some(user.connection()),
            Tls::Kernel(_) | Tls::Failed(_) => None,
        }
    }

    /// Converts the connection to kernel TLS, once everything that ties it to user space is gone. It stays in user
    /// space (`false`) when the kernel was not offered or cannot run the negotiated suite, or when the peer has sent
    /// plaintext, half a record or the next request that was not consumed yet. A conversion the kernel refuses after
    /// rustls handed the keys over ends the connection: its next read or write fails.
    fn offload(&mut self, suite: kernel::Suite) -> bool {
        match std::mem::replace(&mut self.tls, Tls::Failed(Failure::default())) {
            Tls::User(user) => self.convert(user, suite),
            other => {
                self.tls = other;
                false
            }
        }
    }

    fn convert(&mut self, mut user: Box<UserTls>, suite: kernel::Suite) -> bool {
        let Some(socket) = self.io.tcp_socket() else {
            self.tls = Tls::User(user);
            return false;
        };
        if let Err(error) = kernel::attach(socket) {
            log::warn!("kernel TLS stays off for a connection: {error}");
            user.keep_in_user_space();
            self.tls = Tls::User(user);
            return false;
        }
        match user.into_kernel(socket, suite) {
            Ok(kernel) => {
                self.tls = Tls::Kernel(Box::new(kernel));
                true
            }
            Err(failure) => {
                log::warn!("{}", failure.error());
                self.tls = Tls::Failed(failure);
                false
            }
        }
    }
}

impl AsyncRead for TlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buffer.remaining() == 0 || this.read_replay(buffer) {
            return Poll::Ready(Ok(()));
        }
        match &mut this.tls {
            Tls::User(user) => user.poll_read(&mut this.io, context, buffer),
            Tls::Kernel(kernel) => kernel.poll_read(&this.io, context, buffer),
            Tls::Failed(failure) => Poll::Ready(Err(failure.error())),
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
        match &mut this.tls {
            Tls::User(user) => user.poll_write(&mut this.io, context, buffer),
            Tls::Kernel(kernel) => kernel.poll_write(&mut this.io, context, buffer),
            Tls::Failed(failure) => Poll::Ready(Err(failure.error())),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match &mut this.tls {
            Tls::User(user) => user.poll_write_vectored(&mut this.io, context, buffers),
            Tls::Kernel(kernel) => kernel.poll_write_vectored(&mut this.io, context, buffers),
            Tls::Failed(failure) => Poll::Ready(Err(failure.error())),
        }
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match &mut this.tls {
            Tls::User(user) => user.poll_flush(&mut this.io, context),
            Tls::Kernel(_) => KernelIo::poll_flush(&mut this.io, context),
            Tls::Failed(failure) => Poll::Ready(Err(failure.error())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match &mut this.tls {
            Tls::User(user) => user.poll_shutdown(&mut this.io, context),
            Tls::Kernel(kernel) => kernel.poll_shutdown(&mut this.io, context),
            Tls::Failed(_) => Pin::new(&mut this.io).poll_shutdown(context),
        }
    }
}

impl UniqueID for TlsStream {
    fn id(&self) -> UniqueIDType {
        self.io.id()
    }
}

impl Ssl for TlsStream {
    fn get_ssl_digest(&self) -> Option<Arc<SslDigest>> {
        Some(Arc::clone(&self.digest))
    }

    fn selected_alpn_proto(&self) -> Option<ALPN> {
        self.alpn.clone()
    }

    fn poll_kernel_tx(&mut self, context: &mut Context<'_>, bytes: u64) -> Poll<bool> {
        if let Tls::User(user) = &mut self.tls {
            let Some(suite) = user.kernel_suite(&self.io) else {
                return Poll::Ready(false);
            };
            if ready!(user.poll_flush(&mut self.io, context)).is_err() || !self.offload(suite) {
                return Poll::Ready(false);
            }
        }
        let Tls::Kernel(kernel) = &mut self.tls else {
            return Poll::Ready(false);
        };
        let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
        kernel
            .poll_sendfile_budget(&self.io, context, bytes)
            .map(|fits| fits.unwrap_or(false))
    }

    fn kernel_tx_socket(&self) -> Option<&TcpStream> {
        match self.tls {
            Tls::Kernel(_) => self.io.tcp_socket(),
            Tls::User(_) | Tls::Failed(_) => None,
        }
    }
}

impl GetTimingDigest for TlsStream {
    fn get_timing_digest(&self) -> Vec<Option<TimingDigest>> {
        let mut digests = self.io.get_timing_digest();
        digests.push(Some(self.timing.clone()));
        digests
    }

    fn get_read_pending_time(&self) -> Duration {
        self.io.get_read_pending_time()
    }

    fn get_write_pending_time(&self) -> Duration {
        self.io.get_write_pending_time()
    }
}

impl GetProxyDigest for TlsStream {
    fn get_proxy_digest(&self) -> Option<Arc<ProxyDigest>> {
        self.io.get_proxy_digest()
    }
}

impl GetSocketDigest for TlsStream {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        self.io.get_socket_digest()
    }

    fn set_socket_digest(&mut self, socket_digest: SocketDigest) {
        self.io.set_socket_digest(socket_digest);
    }
}

#[async_trait]
impl Peek for TlsStream {
    /// A TLS connection ends with its transport, whether the peer sent `close_notify` first or not.
    async fn peer_closed(&mut self) -> io::Result<()> {
        self.io.peer_closed().await
    }

    fn poll_read_ready(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.replayed < self.replay.len() {
            return Poll::Ready(Ok(()));
        }
        match &mut self.tls {
            Tls::User(user) => user.poll_read_ready(&mut self.io, context),
            Tls::Kernel(_) | Tls::Failed(_) => KernelIo::poll_read_ready(&mut self.io, context),
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
