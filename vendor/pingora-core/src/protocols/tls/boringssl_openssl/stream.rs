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

use crate::protocols::digest::TimingDigest;
use crate::protocols::tls::{SslDigest, ALPN};
use crate::protocols::{Peek, Ssl, UniqueID, UniqueIDType};
use crate::tls::{self, ssl, tokio_ssl::SslStream as InnerSsl};
use crate::utils::tls::{get_organization, get_serial};
use log::warn;
use pingora_error::{ErrorType::*, OrErr, Result};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::SystemTime;
use tokio::io::{self, AsyncRead, AsyncWrite, ReadBuf};

#[cfg(feature = "boringssl")]
use pingora_boringssl as ssl_lib;

#[cfg(feature = "openssl")]
use pingora_openssl as ssl_lib;

use ssl_lib::{hash::MessageDigest, ssl::SslRef};

/// Plaintext a coalescing stream keeps for plain writes before the next flush. It matches the
/// accepted socket's write buffer, so data that used to wait for a flush still waits for one and
/// data that used to leave at once still does.
const COALESCE_PLAIN_LIMIT: usize = 1460;
/// Plaintext one vectored write may leave pending: a whole TLS record. Vectored writers (HTTP/2
/// frames, chunked bodies) flush right after.
const COALESCE_VECTORED_LIMIT: usize = 16 * 1024;
/// A drained buffer larger than this is released instead of kept for the next response.
const COALESCE_RETAINED_CAPACITY: usize = 2048;

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
        })
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
        Pin::new(&mut self.ssl).connect().await?;
        self.timing.established_ts = SystemTime::now();
        self.digest = Some(Arc::new(SslDigest::from_ssl(self.ssl())));
        Ok(())
    }

    /// Finish the TLS handshake from client as a server
    pub async fn accept(&mut self) -> Result<(), ssl::Error> {
        Self::clear_error();
        Pin::new(&mut self.ssl).accept().await?;
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
    fn poll_write_pending(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.pending.is_empty() {
            return Poll::Ready(Ok(()));
        }
        Self::clear_error();
        while self.sent < self.pending.len() {
            let written =
                ready!(Pin::new(&mut self.ssl).poll_write(cx, &self.pending[self.sent..]))?;
            self.sent += written;
        }
        self.sent = 0;
        self.pending.clear();
        if self.pending.capacity() > COALESCE_RETAINED_CAPACITY {
            self.pending = Vec::new();
        }
        Poll::Ready(Ok(()))
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
        if let (Poll::Pending, Some(probe)) = (&result, this.read_probe) {
            this.read_blocked = probe(this.ssl.get_mut(), cx).is_pending();
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
        Pin::new(&mut self.ssl).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        ready!(self.poll_write_pending(cx))?;
        Pin::new(&mut self.ssl).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Self::clear_error();
        ready!(self.poll_write_pending(cx))?;
        Pin::new(&mut self.ssl).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.coalesce {
            let total: usize = bufs.iter().map(|buf| buf.len()).sum();
            if self.pending.len() + total > COALESCE_VECTORED_LIMIT {
                ready!(self.poll_write_pending(cx))?;
            }
            if total <= COALESCE_VECTORED_LIMIT {
                self.pending.reserve(total);
                bufs.iter()
                    .for_each(|buf| self.pending.extend_from_slice(buf));
                return Poll::Ready(Ok(total));
            }
        }
        Self::clear_error();
        Pin::new(&mut self.ssl).poll_write_vectored(cx, bufs)
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

impl<T> Ssl for SslStream<T> {
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

#[cfg(all(test, feature = "openssl_derived"))]
#[path = "stream_tests.rs"]
mod tests;
