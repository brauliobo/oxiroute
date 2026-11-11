// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::ktls;
use crate::protocols::digest::TimingDigest;
use crate::protocols::tls::{SslDigest, ALPN};
use crate::protocols::{Peek, Ssl, UniqueID, UniqueIDType};
use crate::tls::{self, ssl, tokio_ssl::SslStream as InnerSsl};
use crate::utils::tls::{get_organization, get_serial};
use log::warn;
use pingora_error::{ErrorType::*, OrErr, Result};
use std::cell::Cell;
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::{Instant, SystemTime};
use tokio::io::{self, AsyncRead, AsyncWrite, Interest, ReadBuf};
use tokio::net::TcpStream;

#[cfg(feature = "boringssl")]
use pingora_boringssl as ssl_lib;

#[cfg(feature = "openssl")]
use pingora_openssl as ssl_lib;

use ssl_lib::{hash::MessageDigest, ssl::SslRef};

/// Plaintext a coalescing stream keeps for plain writes before the next flush. It matches the
/// accepted socket's write buffer, so data that used to wait for a flush still waits for one and
/// data that used to leave at once still does.
const COALESCE_PLAIN_LIMIT: usize = 1460;
/// Plaintext vectored writes may leave pending: a whole TLS record. Vectored writers (HTTP/2
/// frames, chunked bodies) flush right after, and their bytes are packed into full records.
const COALESCE_VECTORED_LIMIT: usize = 16 * 1024;
/// A drained buffer larger than this goes back to the thread's spare instead of staying with the connection.
const COALESCE_RETAINED_CAPACITY: usize = 2048;

thread_local! {
    /// The record-sized buffer of the connections of this thread that are writing a body: a connection
    /// that went idle keeps none.
    static SPARE: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

/// Where an accepted connection is with the kernel TLS offload of its write direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KtlsTx {
    /// Not offered on this connection.
    Off,
    /// Offered: the first caller that asks converts the connection.
    Armed,
    /// The `KeyUpdate` that installs the kernel keys is being written.
    Converting,
    /// The kernel encrypts the records of this connection's writes.
    Active,
    /// Asked for and not possible here (TLS 1.2, a cipher the kernel does not offer, the kernel refused): the
    /// connection stays in user space.
    Declined,
}

/// The TCP socket a transport sits on, as the kernel TLS offload needs it.
type SocketOf<T> = fn(&T) -> Option<&TcpStream>;

/// The TLS connection
#[derive(Debug)]
pub struct SslStream<T> {
    ssl: InnerSsl<T>,
    digest: Option<Arc<SslDigest>>,
    pub(super) timing: TimingDigest,
    /// Plaintext not yet handed to OpenSSL, only used when `coalesce` is set.
    pending: Vec<u8>,
    sent: usize,
    coalesce: bool,
    /// Asks the transport whether a read could make progress, without reading.
    read_probe: Option<ReadProbe<T>>,
    /// The last `SSL_read` ran out of ciphertext and the transport had nothing more either.
    read_blocked: bool,
    ktls: KtlsTx,
    socket_of: Option<SocketOf<T>>,
}

type ReadProbe<T> = fn(&mut T, &mut Context<'_>) -> Poll<io::Result<()>>;

impl<T> SslStream<T>
where
    T: AsyncRead + AsyncWrite + std::marker::Unpin,
{
    /// Create a new TLS connection from the given `stream`
    ///
    /// The caller needs to perform [`Self::connect()`] or [`Self::accept()`] to perform TLS
    /// handshake after.
    pub fn new(ssl: ssl::Ssl, stream: T) -> Result<Self> {
        let ssl = InnerSsl::new(ssl, stream)
            .explain_err(TLSHandshakeFailure, |e| format!("ssl stream error: {e}"))?;

        Ok(SslStream {
            ssl,
            digest: None,
            timing: Default::default(),
            pending: Vec::new(),
            sent: 0,
            coalesce: false,
            read_probe: None,
            read_blocked: false,
            ktls: KtlsTx::Off,
            socket_of: None,
        })
    }

    /// Offer the kernel TLS offload on this connection: the first [`Ssl::poll_kernel_tx`] converts it when the
    /// connection is TLS 1.3 with an AES-GCM cipher and `socket_of` finds a TCP socket under the transport.
    pub(super) fn offer_ktls(&mut self, socket_of: SocketOf<T>) {
        self.ktls = KtlsTx::Armed;
        self.socket_of = Some(socket_of);
    }

    /// Make the writes of this stream share TLS records: bytes written between two flushes leave as
    /// one record instead of one record per write call. Writers must flush when a response is complete,
    /// which accepted streams already need for their socket write buffer.
    pub(super) fn coalesce_writes(&mut self) {
        self.coalesce = true;
    }

    /// Skip `SSL_read` while the transport still has nothing to read after a read that ran dry: the
    /// call could only run the record layer again to report the same "want read".
    pub(super) fn probe_reads(&mut self, probe: ReadProbe<T>) {
        self.read_probe = Some(probe);
    }

    /// Connect to the remote TLS server as a client
    pub async fn connect(&mut self) -> Result<(), ssl::Error> {
        Self::clear_error();
        let handshake_start = Instant::now();
        Pin::new(&mut self.ssl).connect().await?;
        self.timing.establishment_duration = Some(handshake_start.elapsed());
        self.timing.established_ts = SystemTime::now();
        self.digest = Some(Arc::new(SslDigest::from_ssl(self.ssl())));
        Ok(())
    }

    /// Finish the TLS handshake from client as a server
    pub async fn accept(&mut self) -> Result<(), ssl::Error> {
        Self::clear_error();
        let handshake_start = Instant::now();
        Pin::new(&mut self.ssl).accept().await?;
        self.timing.establishment_duration = Some(handshake_start.elapsed());
        self.timing.established_ts = SystemTime::now();
        self.digest = Some(Arc::new(SslDigest::from_ssl(self.ssl())));
        Ok(())
    }

    #[inline]
    fn clear_error() {
        let errs = tls::error::ErrorStack::get();
        if !errs.errors().is_empty() {
            warn!("Clearing dirty TLS error stack: {}", errs);
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> SslStream<T> {
    /// Moves as much of `bufs` as fits the record being filled into `pending`; the next call, or the
    /// flush the writer issues at the end of its batch, sends it.
    fn poll_write_vectored_pending(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.pending.len() >= COALESCE_VECTORED_LIMIT {
            ready!(self.poll_write_pending(cx))?;
        }
        if self.pending.capacity() == 0 {
            self.pending = SPARE.take();
        }
        let room = COALESCE_VECTORED_LIMIT - self.pending.len();
        self.pending.reserve(room);
        let mut taken = 0;
        for buf in bufs {
            let part = &buf[..buf.len().min(room - taken)];
            self.pending.extend_from_slice(part);
            taken += part.len();
        }
        Poll::Ready(Ok(taken))
    }

    fn poll_write_pending(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.pending.is_empty() {
            return Poll::Ready(Ok(()));
        }
        Self::clear_error();
        while self.sent < self.pending.len() {
            let result = Pin::new(&mut self.ssl).poll_write(cx, &self.pending[self.sent..]);
            self.wake_for_write(cx, &result);
            self.sent += ready!(result)?;
        }
        self.sent = 0;
        self.pending.clear();
        if self.pending.capacity() > COALESCE_RETAINED_CAPACITY {
            SPARE.set(std::mem::take(&mut self.pending));
        }
        Poll::Ready(Ok(()))
    }

    /// The TCP socket under the transport.
    fn ktls_socket(&self) -> Option<&TcpStream> {
        (self.socket_of?)(self.ssl.get_ref())
    }

    /// Once the kernel sends this connection's records, OpenSSL writes to the descriptor itself and a full socket
    /// buffer reaches no waker: ask the socket to wake the task when it can take more bytes.
    fn wake_for_write<R>(&self, cx: &mut Context<'_>, result: &Poll<R>) {
        if !matches!(self.ktls, KtlsTx::Converting | KtlsTx::Active)
            || result.is_ready()
            || !ktls::wants_write(self.ssl.ssl())
        {
            return;
        }
        let Some(socket) = self.ktls_socket() else {
            return;
        };
        // The write that met the full buffer did not go through the socket's own readiness tracking.
        let _ = socket.try_io(Interest::WRITABLE, || {
            Err::<(), _>(io::ErrorKind::WouldBlock.into())
        });
        if socket.poll_write_ready(cx).is_ready() {
            cx.waker().wake_by_ref();
        }
    }

    /// Whether the kernel offers what this connection negotiated: TLS 1.3 with AES-GCM (TLS 1.2 has no key update
    /// to install the kernel keys with; the other suites are not part of the offload).
    fn ktls_negotiated(&self) -> bool {
        let ssl = self.ssl.ssl();
        ssl.version2() == Some(ssl::SslVersion::TLS1_3)
            && ssl.current_cipher().is_some_and(|cipher| {
                matches!(
                    cipher.name(),
                    "TLS_AES_128_GCM_SHA256" | "TLS_AES_256_GCM_SHA384"
                )
            })
    }

    /// Drives the conversion of the write direction to the kernel TLS offload: `Ready(true)` when the kernel sends
    /// the records of this connection, `Ready(false)` when it stays in user space.
    pub(super) fn poll_ktls_tx(&mut self, cx: &mut Context<'_>) -> Poll<bool> {
        loop {
            match self.ktls {
                KtlsTx::Off | KtlsTx::Declined => return Poll::Ready(false),
                KtlsTx::Active => return Poll::Ready(true),
                KtlsTx::Armed => {
                    let socket = self.ktls_socket().map(AsRawFd::as_raw_fd);
                    let Some(fd) = socket.filter(|_| self.ktls_negotiated()) else {
                        self.ktls = KtlsTx::Declined;
                        continue;
                    };
                    // Nothing may wait in user space when the keys change hands.
                    let mut flushed = ready!(self.poll_write_pending(cx));
                    if flushed.is_ok() {
                        flushed = ready!(Pin::new(&mut self.ssl).poll_flush(cx));
                    }
                    if flushed.is_err() {
                        self.ktls = KtlsTx::Declined;
                        continue;
                    }
                    let scheduled = ktls::begin_tx(self.ssl.ssl(), fd);
                    self.ktls = if scheduled {
                        KtlsTx::Converting
                    } else {
                        KtlsTx::Declined
                    };
                }
                KtlsTx::Converting => {
                    let result = Pin::new(&mut self.ssl).poll_do_handshake(cx);
                    self.wake_for_write(cx, &result);
                    let done = ready!(result);
                    if done.is_ok() && ktls::tx_active(self.ssl.ssl()) {
                        self.ktls = KtlsTx::Active;
                        continue;
                    }
                    // The key update went out with the user-space keys: either the kernel refused its half (the
                    // connection goes on as it was) or the stream is broken (the next read or write reports it).
                    self.ktls = KtlsTx::Declined;
                    ktls::restore_wbio(self.ssl.ssl());
                }
            }
        }
    }
}

impl<T> SslStream<T> {
    pub fn ssl_digest(&self) -> Option<Arc<SslDigest>> {
        self.digest.clone()
    }

    /// Attempts to obtain a mutable reference to the SslDigest.
    /// This method returns `None` if the SslDigest is currently held by other references.
    pub(crate) fn ssl_digest_mut(&mut self) -> Option<&mut SslDigest> {
        Arc::get_mut(self.digest.as_mut()?)
    }
}

use std::ops::{Deref, DerefMut};

impl<T> Deref for SslStream<T> {
    type Target = InnerSsl<T>;

    fn deref(&self) -> &Self::Target {
        &self.ssl
    }
}

impl<T> DerefMut for SslStream<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ssl
    }
}

impl<T> AsyncRead for SslStream<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.read_blocked {
            let probe = this.read_probe.expect("a blocked read has a probe");
            if probe(this.ssl.get_mut(), cx).is_pending() {
                return Poll::Pending;
            }
            this.read_blocked = false;
        }
        Self::clear_error();
        let result = Pin::new(&mut this.ssl).poll_read(cx, buf);
        this.wake_for_write(cx, &result);
        // A read that waits for room to write (the answer to a `KeyUpdate`) is not waiting for ciphertext.
        let wants_write = this.ktls != KtlsTx::Off && ktls::wants_write(this.ssl.ssl());
        if let (Poll::Pending, Some(probe)) = (&result, this.read_probe) {
            this.read_blocked = !wants_write && probe(this.ssl.get_mut(), cx).is_pending();
        }
        result
    }
}

impl<T> AsyncWrite for SslStream<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.coalesce {
            if self.pending.len() + buf.len() > COALESCE_PLAIN_LIMIT {
                ready!(self.poll_write_pending(cx))?;
            }
            if buf.len() <= COALESCE_PLAIN_LIMIT {
                self.pending.extend_from_slice(buf);
                return Poll::Ready(Ok(buf.len()));
            }
        }
        Self::clear_error();
        let result = Pin::new(&mut self.ssl).poll_write(cx, buf);
        self.wake_for_write(cx, &result);
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        ready!(self.poll_write_pending(cx))?;
        Pin::new(&mut self.ssl).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Self::clear_error();
        ready!(self.poll_write_pending(cx))?;
        let result = Pin::new(&mut self.ssl).poll_shutdown(cx);
        self.wake_for_write(cx, &result);
        result
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.coalesce {
            return self.poll_write_vectored_pending(cx, bufs);
        }
        Self::clear_error();
        let result = Pin::new(&mut self.ssl).poll_write_vectored(cx, bufs);
        self.wake_for_write(cx, &result);
        result
    }

    fn is_write_vectored(&self) -> bool {
        true
    }
}

impl<T> UniqueID for SslStream<T>
where
    T: UniqueID,
{
    fn id(&self) -> UniqueIDType {
        self.ssl.get_ref().id()
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> Ssl for SslStream<T> {
    fn get_ssl(&self) -> Option<&ssl::SslRef> {
        Some(self.ssl())
    }

    fn get_ssl_digest(&self) -> Option<Arc<SslDigest>> {
        self.ssl_digest()
    }

    /// Return selected ALPN if any
    fn selected_alpn_proto(&self) -> Option<ALPN> {
        let ssl = self.get_ssl()?;
        ALPN::from_wire_selected(ssl.selected_alpn_protocol()?)
    }

    fn poll_kernel_tx(&mut self, cx: &mut Context<'_>) -> Poll<bool> {
        self.poll_ktls_tx(cx)
    }

    fn kernel_tx_socket(&self) -> Option<&TcpStream> {
        self.ktls_socket().filter(|_| self.ktls == KtlsTx::Active)
    }
}

impl SslDigest {
    pub fn from_ssl(ssl: &SslRef) -> Self {
        let cipher = match ssl.current_cipher() {
            Some(c) => c.name(),
            None => "",
        };

        let (cert_digest, org, sn) = match ssl.peer_certificate() {
            Some(cert) => {
                let cert_digest = match cert.digest(MessageDigest::sha256()) {
                    Ok(c) => c.as_ref().to_vec(),
                    Err(_) => Vec::new(),
                };
                (cert_digest, get_organization(&cert), get_serial(&cert).ok())
            }
            None => (Vec::new(), None, None),
        };

        SslDigest::new(cipher, ssl.version_str(), org, sn, cert_digest)
    }
}

// TODO: implement Peek if needed
impl<T> Peek for SslStream<T> {}

#[cfg(all(test, feature = "openssl"))]
#[path = "stream_tests.rs"]
mod tests;
