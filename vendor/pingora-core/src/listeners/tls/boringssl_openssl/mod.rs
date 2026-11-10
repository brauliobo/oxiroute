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

use log::debug;
use pingora_error::{Error, ErrorType, OrErr, Result};
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinHandle;

use crate::listeners::tls::boringssl_openssl::alpn::valid_alpn;
pub use crate::protocols::tls::ALPN;
use crate::protocols::IO;
use crate::tls::ssl::AlpnError;
use crate::tls::ssl::{SslAcceptor, SslAcceptorBuilder, SslFiletype, SslMethod};
use crate::{
    listeners::TlsAcceptCallbacks,
    protocols::tls::{
        server::{handshake, handshake_with_callback},
        SslStream,
    },
};

#[cfg(feature = "openssl")]
unsafe extern "C" {
    // openssl-sys does not declare this exported OpenSSL function.
    fn SSL_CTX_set_timeout(
        context: *mut crate::tls::ssl_sys::SSL_CTX,
        timeout_seconds: libc::c_long,
    ) -> libc::c_long;
}
pub const TLS_CONF_ERR: ErrorType = ErrorType::Custom("TLSConfigError");

/// Runs server side TLS handshakes on threads other than the one that owns the connection, so the
/// key exchange and signature (hundreds of microseconds of CPU each) do not stall the connections
/// that are being served on that thread.
pub trait HandshakeExecutor: Send + Sync {
    /// Starts `job`. `None` refuses it (the executor's queue is full) and the connection is dropped.
    /// The job is aborted through the returned handle when the handshake is given up.
    fn spawn(&self, job: Pin<Box<dyn Future<Output = ()> + Send>>) -> Option<JoinHandle<()>>;
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) struct Acceptor {
    ssl_acceptor: SslAcceptor,
    callbacks: Option<Arc<TlsAcceptCallbacks>>,
    executor: Option<Arc<dyn HandshakeExecutor>>,
}

/// The TLS settings of a listening endpoint
pub struct TlsSettings {
    accept_builder: SslAcceptorBuilder,
    callbacks: Option<TlsAcceptCallbacks>,
    executor: Option<Arc<dyn HandshakeExecutor>>,
}

impl From<SslAcceptorBuilder> for TlsSettings {
    fn from(settings: SslAcceptorBuilder) -> Self {
        TlsSettings {
            accept_builder: settings,
            callbacks: None,
            executor: None,
        }
    }
}

impl Deref for TlsSettings {
    type Target = SslAcceptorBuilder;

    fn deref(&self) -> &Self::Target {
        &self.accept_builder
    }
}

impl DerefMut for TlsSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.accept_builder
    }
}

impl TlsSettings {
    /// Create a new [`TlsSettings`] with the [Mozilla Intermediate](https://wiki.mozilla.org/Security/Server_Side_TLS#Intermediate_compatibility_.28recommended.29)
    /// server side TLS settings. Users can adjust the TLS settings after this object is created.
    /// Return error if the provided certificate and private key are invalid or not found.
    pub fn intermediate(cert_path: &str, key_path: &str) -> Result<Self> {
        let mut accept_builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).or_err(
            TLS_CONF_ERR,
            "fail to create mozilla_intermediate_v5 Acceptor",
        )?;
        accept_builder
            .set_private_key_file(key_path, SslFiletype::PEM)
            .or_err_with(TLS_CONF_ERR, || format!("fail to read key file {key_path}"))?;
        accept_builder
            .set_certificate_chain_file(cert_path)
            .or_err_with(TLS_CONF_ERR, || {
                format!("fail to read cert file {cert_path}")
            })?;
        Ok(TlsSettings {
            accept_builder,
            callbacks: None,
            executor: None,
        })
    }

    /// Create a new [`TlsSettings`] similar to [TlsSettings::intermediate()]. A struct that implements [TlsAcceptCallbacks]
    /// is needed to provide the certificate during the TLS handshake.
    pub fn with_callbacks(callbacks: TlsAcceptCallbacks) -> Result<Self> {
        let accept_builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).or_err(
            TLS_CONF_ERR,
            "fail to create mozilla_intermediate_v5 Acceptor",
        )?;
        Ok(TlsSettings {
            accept_builder,
            callbacks: Some(callbacks),
            executor: None,
        })
    }

    /// Run this endpoint's handshakes on `executor` instead of the accepting thread.
    pub fn set_handshake_executor(&mut self, executor: Arc<dyn HandshakeExecutor>) {
        self.executor = Some(executor);
    }

    /// Enable HTTP/2 support for this endpoint, which is default off.
    /// This effectively sets the ALPN to prefer HTTP/2 with HTTP/1.1 allowed
    pub fn enable_h2(&mut self) {
        self.set_alpn(ALPN::H2H1);
    }

    /// Set the ALPN preference of this endpoint. See [`ALPN`] for more details
    pub fn set_alpn(&mut self, alpn: ALPN) {
        match alpn {
            ALPN::H2H1 => self
                .accept_builder
                .set_alpn_select_callback(alpn::prefer_h2),
            ALPN::H1 => self.accept_builder.set_alpn_select_callback(alpn::h1_only),
            ALPN::H2 => self.accept_builder.set_alpn_select_callback(alpn::h2_only),
            ALPN::Custom(custom) => {
                self.accept_builder
                    .set_alpn_select_callback(move |_, alpn_in| {
                        if !valid_alpn(alpn_in) {
                            return Err(AlpnError::NOACK);
                        }
                        match alpn::select_protocol(alpn_in, custom.protocol()) {
                            Some(p) => Ok(p),
                            None => Err(AlpnError::NOACK),
                        }
                    });
            }
        }
    }

    /// Set the server session timeout in exact seconds, returning the previous timeout.
    #[cfg(feature = "openssl")]
    pub fn set_session_timeout(&mut self, timeout_seconds: i32) -> i64 {
        use foreign_types::ForeignTypeRef as _;

        let context = unsafe {
            // SAFETY: the acceptor builder owns this live SSL_CTX for the duration of the call.
            crate::tls::ssl::SslContextRef::from_ptr(self.accept_builder.as_ptr())
        };
        unsafe {
            // SAFETY: `context` points to the builder's live SSL_CTX and the positive i32 timeout
            // converts losslessly to every supported platform's c_long.
            SSL_CTX_set_timeout(context.as_ptr(), libc::c_long::from(timeout_seconds)) as i64
        }
    }

    pub(crate) fn build(self) -> Acceptor {
        Acceptor {
            ssl_acceptor: self.accept_builder.build(),
            callbacks: self.callbacks.map(Arc::new),
            executor: self.executor,
        }
    }
}

impl Acceptor {
    pub async fn tls_handshake<S: IO + 'static>(&self, stream: S) -> Result<SslStream<S>> {
        debug!("new ssl session");
        let Some(executor) = self.executor.as_ref() else {
            return Self::accept(&self.ssl_acceptor, self.callbacks.as_deref(), stream).await;
        };
        let (done, result) = tokio::sync::oneshot::channel();
        let ssl_acceptor = self.ssl_acceptor.clone();
        let callbacks = self.callbacks.clone();
        let job = Box::pin(async move {
            let accepted = Self::accept(&ssl_acceptor, callbacks.as_deref(), stream).await;
            let _ = done.send(accepted);
        });
        let task = executor.spawn(job).ok_or_else(|| {
            Error::explain(
                ErrorType::TLSHandshakeFailure,
                "TLS handshake queue is full",
            )
        })?;
        let _abort = AbortOnDrop(task);
        result
            .await
            .or_err(ErrorType::TLSHandshakeFailure, "TLS handshake was dropped")?
    }

    async fn accept<S: IO>(
        ssl_acceptor: &SslAcceptor,
        callbacks: Option<&TlsAcceptCallbacks>,
        stream: S,
    ) -> Result<SslStream<S>> {
        match callbacks {
            Some(cb) => handshake_with_callback(ssl_acceptor, stream, cb).await,
            None => handshake(ssl_acceptor, stream).await,
        }
    }
}

mod alpn {
    use super::*;
    use crate::tls::ssl::{select_next_proto, AlpnError, SslRef};

    pub(super) fn valid_alpn(alpn_in: &[u8]) -> bool {
        if alpn_in.is_empty() {
            return false;
        }
        // TODO: can add more thorough validation here.
        true
    }

    /// Finds the first protocol in the client-offered ALPN list that matches the given protocol.
    ///
    /// This is a helper for ALPN negotiation. It iterates over the client's protocol list
    /// (in wire format) and returns the first protocol that matches proto
    /// The returned reference always points into `client_protocols`, so lifetimes are correct.
    pub(super) fn select_protocol<'a>(
        client_protocols: &'a [u8],
        proto: &[u8],
    ) -> Option<&'a [u8]> {
        let mut bytes = client_protocols;
        while !bytes.is_empty() {
            let len = bytes[0] as usize;
            bytes = &bytes[1..];
            if len == proto.len() && &bytes[..len] == proto {
                return Some(&bytes[..len]);
            }
            bytes = &bytes[len..];
        }
        None
    }

    // A standard implementation provided by the SSL lib is used below

    pub fn prefer_h2<'a>(_ssl: &mut SslRef, alpn_in: &'a [u8]) -> Result<&'a [u8], AlpnError> {
        if !valid_alpn(alpn_in) {
            return Err(AlpnError::NOACK);
        }
        match select_next_proto(ALPN::H2H1.to_wire_preference(), alpn_in) {
            Some(p) => Ok(p),
            _ => Err(AlpnError::NOACK), // unknown ALPN, just ignore it. Most clients will fallback to h1
        }
    }

    pub fn h1_only<'a>(_ssl: &mut SslRef, alpn_in: &'a [u8]) -> Result<&'a [u8], AlpnError> {
        if !valid_alpn(alpn_in) {
            return Err(AlpnError::NOACK);
        }
        match select_next_proto(ALPN::H1.to_wire_preference(), alpn_in) {
            Some(p) => Ok(p),
            _ => Err(AlpnError::NOACK), // unknown ALPN, just ignore it. Most clients will fallback to h1
        }
    }

    pub fn h2_only<'a>(_ssl: &mut SslRef, alpn_in: &'a [u8]) -> Result<&'a [u8], AlpnError> {
        if !valid_alpn(alpn_in) {
            return Err(AlpnError::ALERT_FATAL);
        }
        match select_next_proto(ALPN::H2.to_wire_preference(), alpn_in) {
            Some(p) => Ok(p),
            _ => Err(AlpnError::ALERT_FATAL), // cannot agree
        }
    }
}

#[cfg(all(test, feature = "openssl"))]
mod handshake_executor_tests {
    use super::*;
    use crate::protocols::tls::SslStream;
    use crate::tls::ssl;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, DuplexStream};

    /// Runs jobs on a runtime whose only thread is named `handshake-test`.
    struct Pool {
        runtime: Option<tokio::runtime::Runtime>,
        refuse: AtomicBool,
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            // The test's own runtime is dropping this: a runtime cannot be dropped from async code.
            self.runtime.take().unwrap().shutdown_background();
        }
    }

    impl Pool {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                runtime: Some(
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .thread_name("handshake-test")
                        .build()
                        .unwrap(),
                ),
                refuse: AtomicBool::new(false),
            })
        }
    }

    impl HandshakeExecutor for Pool {
        fn spawn(&self, job: Pin<Box<dyn Future<Output = ()> + Send>>) -> Option<JoinHandle<()>> {
            (!self.refuse.load(Ordering::Relaxed))
                .then(|| self.runtime.as_ref().unwrap().spawn(job))
        }
    }

    #[derive(Clone, Default)]
    struct ThreadProbe(Arc<std::sync::Mutex<Vec<String>>>);

    #[async_trait::async_trait]
    impl crate::listeners::TlsAccept for ThreadProbe {
        async fn certificate_callback(&self, ssl: &mut crate::protocols::tls::TlsRef) {
            let cert = crate::test_tls::TestCert::new();
            let chain = std::fs::read(&cert.cert_path).unwrap();
            let key = std::fs::read(&cert.key_path).unwrap();
            let chain = crate::tls::x509::X509::from_pem(&chain).unwrap();
            let key = crate::tls::pkey::PKey::private_key_from_pem(&key).unwrap();
            crate::tls::ext::ssl_use_certificate(ssl, &chain).unwrap();
            crate::tls::ext::ssl_use_private_key(ssl, &key).unwrap();
            let name = std::thread::current().name().unwrap_or_default().to_owned();
            self.0.lock().unwrap().push(name);
        }
    }

    fn acceptor(executor: Arc<Pool>, probe: &ThreadProbe) -> Acceptor {
        let mut settings = TlsSettings::with_callbacks(Box::new(probe.clone())).unwrap();
        settings.set_handshake_executor(executor);
        settings.build()
    }

    async fn client(io: DuplexStream) {
        let context = ssl::SslContext::builder(ssl::SslMethod::tls())
            .unwrap()
            .build();
        let mut session = ssl::Ssl::new(&context).unwrap();
        session.set_hostname("pingora.org").unwrap();
        session.set_verify(ssl::SslVerifyMode::NONE);
        let mut stream = SslStream::new(session, io).unwrap();
        Pin::new(&mut stream).connect().await.unwrap();
        let _ = stream.read(&mut [0; 1]).await;
    }

    #[tokio::test]
    async fn handshake_runs_on_the_executor_thread_and_the_stream_returns() {
        let pool = Pool::new();
        let probe = ThreadProbe::default();
        let acceptor = acceptor(pool, &probe);
        let (peer, server) = tokio::io::duplex(1024);
        tokio::spawn(client(peer));
        let stream = acceptor.tls_handshake(server).await.unwrap();
        assert_eq!(
            stream.ssl().servername(ssl::NameType::HOST_NAME),
            Some("pingora.org")
        );
        assert_eq!(*probe.0.lock().unwrap(), ["handshake-test"]);
    }

    #[tokio::test]
    async fn refused_handshake_fails_the_connection() {
        let pool = Pool::new();
        pool.refuse.store(true, Ordering::Relaxed);
        let acceptor = acceptor(pool, &ThreadProbe::default());
        let (_peer, server) = tokio::io::duplex(1024);
        let error = acceptor.tls_handshake(server).await.err().unwrap();
        assert_eq!(*error.etype(), ErrorType::TLSHandshakeFailure);
    }

    #[tokio::test]
    async fn abandoned_handshake_aborts_the_job() {
        let pool = Pool::new();
        let acceptor = acceptor(pool, &ThreadProbe::default());
        let (peer, server) = tokio::io::duplex(1024);
        // The peer never speaks: the job waits for a ClientHello until the caller gives up.
        let attempt = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            acceptor.tls_handshake(server),
        );
        assert!(attempt.await.is_err());
        // The aborted job dropped the server half, so the peer sees the end of the stream.
        let mut peer = peer;
        let mut byte = [0; 1];
        let read =
            tokio::time::timeout(std::time::Duration::from_secs(5), peer.read(&mut byte)).await;
        assert_eq!(read.unwrap().unwrap(), 0);
    }
}
