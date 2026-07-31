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

//! Connecting to servers

pub mod http;
pub mod l4;
mod offload;

#[cfg(feature = "any_tls")]
mod tls;

#[cfg(not(feature = "any_tls"))]
use crate::tls::connectors as tls;

use crate::protocols::{SocketDigest, Stream};
use crate::server::configuration::ServerConf;
use crate::upstreams::peer::{Peer, ALPN};

pub use l4::Connect as L4Connect;
use l4::{connect as l4_connect, BindTo};
use log::{debug, error, warn};
use offload::OffloadRuntime;
use parking_lot::RwLock;
use pingora_error::{Error, ErrorType::*, OrErr, Result};
use pingora_pool::{ConnectionMeta, ConnectionPool};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tls::TlsConnector;
use tokio::sync::Mutex;

/// The options to configure a [TransportConnector]
#[derive(Clone)]
pub struct ConnectorOptions {
    /// Path to the CA file used to validate server certs.
    ///
    /// If `None`, the CA in the [default](https://www.openssl.org/docs/manmaster/man3/SSL_CTX_set_default_verify_paths.html)
    /// locations will be loaded
    pub ca_file: Option<String>,
    /// The maximum number of unique s2n configs to cache. Creating a new s2n config is an
    /// expensive operation, so we cache and re-use config objects with identical configurations.
    /// Defaults to a cache size of 10. A value of 0 disables the cache.
    ///
    /// WARNING: Disabling the s2n config cache can result in poor performance
    #[cfg(feature = "s2n")]
    pub s2n_config_cache_size: Option<usize>,
    /// The default client cert and key to use for mTLS
    ///
    /// Each individual connection can use their own cert key to override this.
    pub cert_key_file: Option<(String, String)>,
    /// When enabled allows TLS keys to be written to a file specified by the SSLKEYLOG
    /// env variable. This can be used by tools like Wireshark to decrypt traffic
    /// for debugging purposes.
    pub debug_ssl_keylog: bool,
    /// How many connections to keepalive
    pub keepalive_pool_size: usize,
    /// Optionally offload the connection establishment to dedicated thread pools
    ///
    /// TCP and TLS connection establishment can be CPU intensive. Sometimes such tasks can slow
    /// down the entire service, which causes timeouts which leads to more connections which
    /// snowballs the issue. Use this option to isolate these CPU intensive tasks from impacting
    /// other traffic.
    ///
    /// Syntax: (#pools, #thread in each pool)
    pub offload_threadpool: Option<(usize, usize)>,
    /// Bind to any of the given source IPv6 addresses
    pub bind_to_v4: Vec<SocketAddr>,
    /// Bind to any of the given source IPv4 addresses
    pub bind_to_v6: Vec<SocketAddr>,
}

impl ConnectorOptions {
    /// Derive the [ConnectorOptions] from a [ServerConf]
    pub fn from_server_conf(server_conf: &ServerConf) -> Self {
        // if both pools and threads are Some(>0)
        let offload_threadpool = server_conf
            .upstream_connect_offload_threadpools
            .zip(server_conf.upstream_connect_offload_thread_per_pool)
            .filter(|(pools, threads)| *pools > 0 && *threads > 0);

        // create SocketAddrs with port 0 for src addr bind

        let bind_to_v4 = server_conf
            .client_bind_to_ipv4
            .iter()
            .map(|v4| {
                let ip = v4.parse().unwrap();
                SocketAddr::new(ip, 0)
            })
            .collect();

        let bind_to_v6 = server_conf
            .client_bind_to_ipv6
            .iter()
            .map(|v6| {
                let ip = v6.parse().unwrap();
                SocketAddr::new(ip, 0)
            })
            .collect();
        ConnectorOptions {
            ca_file: server_conf.ca_file.clone(),
            cert_key_file: None, // TODO: use it
            #[cfg(feature = "s2n")]
            s2n_config_cache_size: server_conf.s2n_config_cache_size,
            debug_ssl_keylog: server_conf.upstream_debug_ssl_keylog,
            keepalive_pool_size: server_conf.upstream_keepalive_pool_size,
            offload_threadpool,
            bind_to_v4,
            bind_to_v6,
        }
    }

    /// Create a new [ConnectorOptions] with the given keepalive pool size
    pub fn new(keepalive_pool_size: usize) -> Self {
        ConnectorOptions {
            ca_file: None,
            #[cfg(feature = "s2n")]
            s2n_config_cache_size: None,
            cert_key_file: None,
            debug_ssl_keylog: false,
            keepalive_pool_size,
            offload_threadpool: None,
            bind_to_v4: vec![],
            bind_to_v6: vec![],
        }
    }
}

/// [TransportConnector] provides APIs to connect to servers via TCP or TLS with connection reuse
pub struct TransportConnector {
    tls_ctx: tls::Connector,
    connection_pool: Arc<ConnectionPool<Arc<Mutex<Stream>>>>,
    offload: Option<OffloadRuntime>,
    bind_to_v4: Vec<SocketAddr>,
    bind_to_v6: Vec<SocketAddr>,
    preferred_http_version: PreferredHttpVersion,
}

const DEFAULT_POOL_SIZE: usize = 128;

impl TransportConnector {
    /// Create a new [TransportConnector] with the given [ConnectorOptions]
    pub fn new(mut options: Option<ConnectorOptions>) -> Self {
        let pool_size = options
            .as_ref()
            .map_or(DEFAULT_POOL_SIZE, |c| c.keepalive_pool_size);
        // Take the offloading setting there because this layer has implement offloading,
        // so no need for stacks at lower layer to offload again.
        let offload = options.as_mut().and_then(|o| o.offload_threadpool.take());
        let bind_to_v4 = options
            .as_ref()
            .map_or_else(Vec::new, |o| o.bind_to_v4.clone());
        let bind_to_v6 = options
            .as_ref()
            .map_or_else(Vec::new, |o| o.bind_to_v6.clone());
        TransportConnector {
            tls_ctx: tls::Connector::new(options),
            connection_pool: Arc::new(ConnectionPool::new(pool_size)),
            offload: offload.map(|v| OffloadRuntime::new(v.0, v.1)),
            bind_to_v4,
            bind_to_v6,
            preferred_http_version: PreferredHttpVersion::new(),
        }
    }

    /// Connect to the given server [Peer]
    ///
    /// No connection is reused.
    pub async fn new_stream<P: Peer + Send + Sync + 'static>(&self, peer: &P) -> Result<Stream> {
        let lifetime = peer.connection_lifetime();
        if let Some(lifetime) = &lifetime {
            loop {
                let generation = lifetime.capacity_generation();
                if lifetime.try_acquire()? {
                    break;
                }
                lifetime.wait_for_capacity(generation).await?;
            }
        }
        self.new_stream_with_lifetime(peer, lifetime).await
    }

    pub(crate) async fn new_stream_with_lifetime<P: Peer + Send + Sync + 'static>(
        &self,
        peer: &P,
        lifetime: Option<Arc<dyn crate::protocols::ConnectionLifetime>>,
    ) -> Result<Stream> {
        let rt = self
            .offload
            .as_ref()
            .map(|o| o.get_runtime(peer.reuse_hash()));
        let bind_to = l4::bind_to_random(peer, &self.bind_to_v4, &self.bind_to_v6);
        let alpn_override = self.preferred_http_version.get(peer);
        let stream = if let Some(rt) = rt {
            let peer = peer.clone();
            let tls_ctx = self.tls_ctx.clone();
            rt.spawn(async move { do_connect(&peer, bind_to, alpn_override, &tls_ctx.ctx).await })
                .await
                .or_err(InternalError, "offload runtime failure")??
        } else {
            do_connect(peer, bind_to, alpn_override, &self.tls_ctx.ctx).await?
        };

        if let Some(lifetime) = lifetime {
            let digest = stream.get_socket_digest().ok_or_else(|| {
                Error::explain(
                    InternalError,
                    "upstream connection has no socket lifetime metadata",
                )
            })?;
            digest.attach_connection_lifetime(lifetime).map_err(|_| {
                Error::explain(
                    InternalError,
                    "upstream connection lifetime metadata was already attached",
                )
            })?;
        }

        Ok(stream)
    }

    /// Try to find a reusable connection to the given server [Peer]
    pub async fn reused_stream<P: Peer + Send + Sync>(&self, peer: &P) -> Option<Stream> {
        match self.connection_pool.get(&peer.reuse_hash()) {
            Some(s) => {
                debug!("find reusable stream, trying to acquire it");
                {
                    let _ = s.lock().await;
                } // wait for the idle poll to release it
                match Arc::try_unwrap(s) {
                    Ok(l) => {
                        let mut stream = l.into_inner();
                        // test_reusable_stream: we assume server would never actively send data
                        // first on an idle stream.
                        #[cfg(unix)]
                        if cached_peer_addr_match(peer, stream.get_socket_digest().as_deref())
                            .unwrap_or_else(|| peer.matches_fd(stream.id()))
                            && test_reusable_stream(&mut stream)
                        {
                            Some(stream)
                        } else {
                            None
                        }
                        #[cfg(windows)]
                        {
                            use std::os::windows::io::{AsRawSocket, RawSocket};
                            struct WrappedRawSocket(RawSocket);
                            impl AsRawSocket for WrappedRawSocket {
                                fn as_raw_socket(&self) -> RawSocket {
                                    self.0
                                }
                            }
                            if cached_peer_addr_match(peer, stream.get_socket_digest().as_deref())
                                .unwrap_or_else(|| {
                                    peer.matches_sock(WrappedRawSocket(stream.id() as RawSocket))
                                })
                                && test_reusable_stream(&mut stream)
                            {
                                Some(stream)
                            } else {
                                None
                            }
                        }
                    }
                    Err(_) => {
                        error!("failed to acquire reusable stream");
                        None
                    }
                }
            }
            None => {
                debug!("No reusable connection found for {peer}");
                None
            }
        }
    }

    /// Return the [Stream] to the [TransportConnector] for connection reuse.
    ///
    /// Not all TCP/TLS connections can be reused. It is the caller's responsibility to make sure
    /// that protocol over the [Stream] supports connection reuse and the [Stream] itself is ready
    /// to be reused.
    ///
    /// If a [Stream] is dropped instead of being returned via this function. it will be closed.
    pub fn release_stream(
        &self,
        stream: Stream,
        key: u64, // usually peer.reuse_hash()
        idle_timeout: Option<std::time::Duration>,
    ) {
        let lifetime = stream
            .get_socket_digest()
            .and_then(|digest| digest.connection_lifetime());
        let id = stream.id();
        let meta = ConnectionMeta::new(key, id);
        debug!("Try to keepalive client session");
        let stream = Arc::new(Mutex::new(stream));
        let locked_stream = stream.clone().try_lock_owned().unwrap(); // safe as we just created it
        let (notify_close, watch_use) = self.connection_pool.put(&meta, stream);
        if let Some(lifetime) = lifetime {
            lifetime.notify_reusable();
        }
        let pool = self.connection_pool.clone(); //clone the arc
        let rt = pingora_runtime::current_handle();
        rt.spawn(async move {
            pool.idle_poll(locked_stream, &meta, idle_timeout, notify_close, watch_use)
                .await;
        });
    }

    /// Get a stream to the given server [Peer]
    ///
    /// This function will try to find a reusable [Stream] first. If there is none, a new connection
    /// will be made to the server.
    ///
    /// The returned boolean will indicate whether the stream is reused.
    pub async fn get_stream<P: Peer + Send + Sync + 'static>(
        &self,
        peer: &P,
    ) -> Result<(Stream, bool)> {
        let reused_stream = self.reused_stream(peer).await;
        if let Some(s) = reused_stream {
            Ok((s, true))
        } else {
            let s = self.new_stream(peer).await?;
            Ok((s, false))
        }
    }

    /// Tell the connector to always send h1 for ALPN for the given peer in the future.
    pub fn prefer_h1(&self, peer: &impl Peer) {
        self.preferred_http_version.add(peer, 1);
    }
}

pub(crate) fn cached_peer_addr_match<P: Peer>(
    peer: &P,
    digest: Option<&SocketDigest>,
) -> Option<bool> {
    digest?
        .peer_addr
        .get()?
        .as_ref()
        .and_then(|addr| peer.matches_cached_addr(addr))
}

// Perform the actual L4 and tls connection steps while respecting the peer's
// connection timeout if there is one
async fn do_connect<P: Peer + Send + Sync>(
    peer: &P,
    bind_to: Option<BindTo>,
    alpn_override: Option<ALPN>,
    tls_ctx: &TlsConnector,
) -> Result<Stream> {
    // Create the future that does the connections, but don't evaluate it until
    // we decide if we need a timeout or not
    let connect_future = do_connect_inner(peer, bind_to, alpn_override, tls_ctx);

    match peer.total_connection_timeout() {
        Some(t) => match pingora_timeout::timeout(t, connect_future).await {
            Ok(res) => res,
            Err(_) => Error::e_explain(
                ConnectTimedout,
                format!("connecting to server {peer}, total-connection timeout {t:?}"),
            ),
        },
        None => connect_future.await,
    }
}

// Perform the actual L4 and tls connection steps with no timeout
async fn do_connect_inner<P: Peer + Send + Sync>(
    peer: &P,
    bind_to: Option<BindTo>,
    alpn_override: Option<ALPN>,
    tls_ctx: &TlsConnector,
) -> Result<Stream> {
    let stream = l4_connect(peer, bind_to).await?;
    if peer.tls() {
        let tls_stream = tls::connect(stream, peer, alpn_override, tls_ctx).await?;
        Ok(Box::new(tls_stream))
    } else {
        Ok(Box::new(stream))
    }
}

struct PreferredHttpVersion {
    // TODO: shard to avoid the global lock
    versions: RwLock<HashMap<u64, u8>>, // <hash of peer, version>
}

// TODO: limit the size of this

impl PreferredHttpVersion {
    pub fn new() -> Self {
        PreferredHttpVersion {
            versions: RwLock::default(),
        }
    }

    pub fn add(&self, peer: &impl Peer, version: u8) {
        let key = peer.reuse_hash();
        let mut v = self.versions.write();
        v.insert(key, version);
    }

    pub fn get(&self, peer: &impl Peer) -> Option<ALPN> {
        let key = peer.reuse_hash();
        let v = self.versions.read();
        v.get(&key)
            .copied()
            .map(|v| if v == 1 { ALPN::H1 } else { ALPN::H2H1 })
    }
}

use futures::future::FutureExt;
use tokio::io::AsyncReadExt;

/// Test whether a stream is already closed or not reusable (server sent unexpected data)
fn test_reusable_stream(stream: &mut Stream) -> bool {
    let mut buf = [0; 1];
    // tokio::task::unconstrained because now_or_never may yield None when the future is ready
    let result = tokio::task::unconstrained(stream.read(&mut buf[..])).now_or_never();
    if let Some(data_result) = result {
        match data_result {
            Ok(n) => {
                if n == 0 {
                    debug!("Idle connection is closed");
                } else {
                    warn!("Unexpected data read in idle connection");
                }
            }
            Err(e) => {
                debug!("Idle connection is broken: {e:?}");
            }
        }
        false
    } else {
        true
    }
}

/// Test utilities for creating mock acceptors.
#[cfg(all(test, unix))]
pub(crate) mod test_utils {
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;

    /// Generates a unique socket path for testing to avoid conflicts when running in parallel
    pub fn unique_uds_path(test_name: &str) -> String {
        format!(
            "/tmp/test_{test_name}_{:?}_{}.sock",
            std::thread::current().id(),
            std::process::id()
        )
    }

    /// A mock UDS server that accepts one connection, sends data, and waits for shutdown signal
    ///
    /// Returns: (ready_rx, shutdown_tx, server_handle)
    /// - ready_rx: Wait on this to know when server is ready to accept connections
    /// - shutdown_tx: Send on this to tell server to shut down
    /// - server_handle: Join handle for the server task
    pub fn spawn_mock_uds_server(
        socket_path: String,
        response: &'static [u8],
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let server_handle = tokio::spawn(async move {
            let _ = std::fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path).unwrap();
            // Signal that the server is ready to accept connections
            let _ = ready_tx.send(());

            if let Ok((mut stream, _addr)) = listener.accept().await {
                let _ = stream.write_all(response).await;
                // Keep the connection open until the test tells us to shutdown
                let _ = shutdown_rx.await;
            }
            let _ = std::fs::remove_file(&socket_path);
        });

        (ready_rx, shutdown_tx, server_handle)
    }

    /// A mock UDS server that immediately closes connections (for testing error handling)
    ///
    /// Returns: (ready_rx, shutdown_tx, server_handle)
    pub fn spawn_mock_uds_server_close_immediate(
        socket_path: String,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let server_handle = tokio::spawn(async move {
            let _ = std::fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path).unwrap();
            // Signal that the server is ready to accept connections
            let _ = ready_tx.send(());

            if let Ok((mut stream, _addr)) = listener.accept().await {
                let _ = stream.shutdown().await;
                // Wait for shutdown signal before cleaning up
                let _ = shutdown_rx.await;
            }
            let _ = std::fs::remove_file(&socket_path);
        });

        (ready_rx, shutdown_tx, server_handle)
    }
}

#[cfg(test)]
#[cfg(feature = "any_tls")]
mod tests {
    use pingora_error::ErrorType;
    use tls::Connector;

    use super::*;
    use crate::upstreams::peer::BasicPeer;
    #[cfg(unix)]
    use crate::upstreams::peer::HttpPeer;

    #[cfg(unix)]
    use crate::protocols::l4::socket::SocketAddr;
    #[cfg(unix)]
    use crate::protocols::{
        raw_connect::ProxyDigest, ConnectionLifetime, GetProxyDigest, GetSocketDigest,
        GetTimingDigest, Peek, Shutdown, SocketDigest, Ssl, TimingDigest, UniqueID, UniqueIDType,
    };
    #[cfg(unix)]
    use async_trait::async_trait;
    #[cfg(unix)]
    use std::fmt::{Display, Formatter, Result as FmtResult};
    #[cfg(unix)]
    use std::io;
    #[cfg(unix)]
    use std::os::unix::io::AsRawFd;
    #[cfg(unix)]
    use std::pin::Pin;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(unix)]
    use std::task::{Context, Poll};
    #[cfg(unix)]
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    #[cfg(unix)]
    use tokio::sync::Notify;

    #[cfg(unix)]
    #[derive(Clone, Copy, Debug)]
    enum TestRead {
        Pending,
        Eof,
        Data,
        Error,
    }

    #[cfg(unix)]
    #[derive(Debug, Default)]
    struct StreamStats {
        reads: AtomicUsize,
        drops: AtomicUsize,
        read: Notify,
        dropped: Notify,
    }

    #[cfg(unix)]
    #[derive(Debug)]
    struct InstrumentedStream {
        id: UniqueIDType,
        read: TestRead,
        stats: Arc<StreamStats>,
        socket_digest: Option<Arc<SocketDigest>>,
    }

    #[cfg(unix)]
    impl AsyncRead for InstrumentedStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.stats.reads.fetch_add(1, Ordering::SeqCst);
            self.stats.read.notify_one();
            match self.read {
                TestRead::Pending => Poll::Pending,
                TestRead::Eof => Poll::Ready(Ok(())),
                TestRead::Data => {
                    buffer.put_slice(b"x");
                    Poll::Ready(Ok(()))
                }
                TestRead::Error => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "instrumented connection reset",
                ))),
            }
        }
    }

    #[cfg(unix)]
    impl AsyncWrite for InstrumentedStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[cfg(unix)]
    impl Drop for InstrumentedStream {
        fn drop(&mut self) {
            self.stats.drops.fetch_add(1, Ordering::SeqCst);
            self.stats.dropped.notify_one();
        }
    }

    #[cfg(unix)]
    #[async_trait]
    impl Shutdown for InstrumentedStream {
        async fn shutdown(&mut self) {}
    }

    #[cfg(unix)]
    impl UniqueID for InstrumentedStream {
        fn id(&self) -> UniqueIDType {
            self.id
        }
    }

    #[cfg(unix)]
    impl Ssl for InstrumentedStream {}

    #[cfg(unix)]
    impl GetTimingDigest for InstrumentedStream {
        fn get_timing_digest(&self) -> Vec<Option<TimingDigest>> {
            Vec::new()
        }
    }

    #[cfg(unix)]
    impl GetProxyDigest for InstrumentedStream {
        fn get_proxy_digest(&self) -> Option<Arc<ProxyDigest>> {
            None
        }
    }

    #[cfg(unix)]
    impl GetSocketDigest for InstrumentedStream {
        fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
            self.socket_digest.clone()
        }
    }

    #[cfg(unix)]
    impl Peek for InstrumentedStream {}

    #[cfg(unix)]
    #[derive(Clone, Debug)]
    struct PoolTestPeer {
        address: SocketAddr,
        key: u64,
        identity_checks: Arc<AtomicUsize>,
    }

    #[cfg(unix)]
    impl PoolTestPeer {
        fn new(key: u64) -> Self {
            Self {
                address: SocketAddr::Inet("127.0.0.1:1".parse().unwrap()),
                key,
                identity_checks: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[cfg(unix)]
    impl Display for PoolTestPeer {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
            write!(formatter, "{}", self.address)
        }
    }

    #[cfg(unix)]
    impl Peer for PoolTestPeer {
        fn address(&self) -> &SocketAddr {
            &self.address
        }

        fn tls(&self) -> bool {
            false
        }

        fn sni(&self) -> &str {
            ""
        }

        fn reuse_hash(&self) -> u64 {
            self.key
        }

        fn matches_fd<V: AsRawFd>(&self, _fd: V) -> bool {
            self.identity_checks.fetch_add(1, Ordering::SeqCst);
            true
        }
    }

    #[cfg(unix)]
    #[derive(Debug, Default)]
    struct TestLifetime {
        reusable: AtomicUsize,
    }

    #[cfg(unix)]
    #[async_trait]
    impl ConnectionLifetime for TestLifetime {
        fn notify_reusable(&self) {
            self.reusable.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[cfg(unix)]
    fn instrumented_stream(
        id: UniqueIDType,
        read: TestRead,
        lifetime: Option<Arc<dyn ConnectionLifetime>>,
    ) -> (Stream, Arc<StreamStats>) {
        instrumented_stream_with_digest(id, read, lifetime, Some(None))
    }

    #[cfg(unix)]
    fn instrumented_stream_with_digest(
        id: UniqueIDType,
        read: TestRead,
        lifetime: Option<Arc<dyn ConnectionLifetime>>,
        peer_addr: Option<Option<SocketAddr>>,
    ) -> (Stream, Arc<StreamStats>) {
        let stats = Arc::new(StreamStats::default());
        let socket_digest = peer_addr.map(|peer_addr| {
            let socket_digest = Arc::new(SocketDigest::from_raw_fd(id));
            if let Some(peer_addr) = peer_addr {
                socket_digest
                    .peer_addr
                    .set(Some(peer_addr))
                    .expect("empty test peer address");
            }
            if let Some(lifetime) = lifetime {
                socket_digest
                    .attach_connection_lifetime(lifetime)
                    .expect("test connection lifetime attachment");
            }
            socket_digest
        });
        let stream = InstrumentedStream {
            id,
            read,
            stats: stats.clone(),
            socket_digest,
        };
        (Box::new(stream), stats)
    }

    #[cfg(unix)]
    async fn wait_for_drop(stats: &StreamStats) {
        if stats.drops.load(Ordering::SeqCst) == 0 {
            stats.dropped.notified().await;
        }
        assert_eq!(stats.drops.load(Ordering::SeqCst), 1);
    }

    // 192.0.2.1 is effectively a black hole
    const BLACK_HOLE: &str = "192.0.2.1:79";

    #[cfg(unix)]
    #[tokio::test]
    async fn valid_immediate_checkout_runs_one_readiness_probe() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(11);
        let (stream, stats) = instrumented_stream(101, TestRead::Pending, None);

        connector.release_stream(stream, peer.reuse_hash(), None);
        let reused = connector
            .reused_stream(&peer)
            .await
            .expect("immediate reusable stream");

        assert_eq!(reused.id(), 101);
        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert_eq!(peer.identity_checks.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_socket_digest_falls_back_to_custom_identity_check() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(20);
        let (stream, stats) = instrumented_stream_with_digest(110, TestRead::Pending, None, None);

        connector.release_stream(stream, peer.reuse_hash(), None);
        assert!(connector.reused_stream(&peer).await.is_some());

        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert_eq!(peer.identity_checks.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cached_address_still_uses_default_peer_identity_override() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(21);
        let (stream, stats) = instrumented_stream_with_digest(
            111,
            TestRead::Pending,
            None,
            Some(Some(peer.address.clone())),
        );

        connector.release_stream(stream, peer.reuse_hash(), None);
        assert!(connector.reused_stream(&peer).await.is_some());

        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert_eq!(peer.identity_checks.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cached_standard_peer_mismatch_rejects_without_syscall_or_readiness() {
        use crate::protocols::{peer_identity_syscalls, reset_peer_identity_syscalls};

        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = BasicPeer::new("127.0.0.1:8000");
        let cached_addr = SocketAddr::Inet("127.0.0.1:8001".parse().unwrap());
        let (stream, stats) =
            instrumented_stream_with_digest(112, TestRead::Pending, None, Some(Some(cached_addr)));

        connector.release_stream(stream, peer.reuse_hash(), None);
        reset_peer_identity_syscalls();
        assert!(connector.reused_stream(&peer).await.is_none());

        assert_eq!(peer_identity_syscalls(), 0);
        assert_eq!(stats.reads.load(Ordering::SeqCst), 0);
        assert_eq!(stats.drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_tcp_checkout_uses_seeded_physical_peer_without_identity_syscall() {
        use crate::protocols::{peer_identity_syscalls, reset_peer_identity_syscalls};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = shutdown_rx.await;
        });
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = BasicPeer::new(&address.to_string());
        let stream = connector.new_stream(&peer).await.unwrap();
        let digest = stream.get_socket_digest().unwrap();

        assert_eq!(
            digest.peer_addr.get().and_then(Option::as_ref),
            Some(&SocketAddr::Inet(address))
        );

        connector.release_stream(stream, peer.reuse_hash(), None);
        reset_peer_identity_syscalls();
        let reused = connector.reused_stream(&peer).await;

        assert!(reused.is_some());
        assert_eq!(peer_identity_syscalls(), 0);
        drop(reused);
        let _ = shutdown_tx.send(());
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connect_proxy_checkout_uses_seeded_next_hop_without_identity_syscall() {
        use crate::protocols::{peer_identity_syscalls, reset_peer_identity_syscalls};
        use std::collections::BTreeMap;

        let socket_path = test_utils::unique_uds_path("proxy_cached_peer");
        let (ready_rx, shutdown_tx, server_handle) =
            test_utils::spawn_mock_uds_server(socket_path.clone(), b"HTTP/1.1 200 OK\r\n\r\n");
        ready_rx.await.unwrap();

        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = HttpPeer::new_proxy(
            &socket_path,
            "192.0.2.1".parse().unwrap(),
            80,
            false,
            "",
            BTreeMap::new(),
        );
        let stream = connector.new_stream(&peer).await.unwrap();
        let digest = stream.get_socket_digest().unwrap();
        assert_eq!(
            digest
                .peer_addr
                .get()
                .and_then(Option::as_ref)
                .and_then(SocketAddr::as_unix)
                .and_then(std::os::unix::net::SocketAddr::as_pathname),
            Some(std::path::Path::new(&socket_path))
        );

        connector.release_stream(stream, peer.reuse_hash(), None);
        reset_peer_identity_syscalls();
        let reused = connector.reused_stream(&peer).await;

        assert!(reused.is_some());
        assert_eq!(peer_identity_syscalls(), 0);
        drop(reused);
        let _ = shutdown_tx.send(());
        server_handle.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn closed_immediate_checkout_is_rejected_by_checkout_readiness() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(12);
        let (stream, stats) = instrumented_stream(102, TestRead::Eof, None);

        connector.release_stream(stream, peer.reuse_hash(), None);

        assert!(connector.reused_stream(&peer).await.is_none());
        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert_eq!(stats.drops.load(Ordering::SeqCst), 1);
        assert_eq!(peer.identity_checks.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn closed_idle_stream_is_removed_without_checkout() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(13);
        let (stream, stats) = instrumented_stream(103, TestRead::Eof, None);

        connector.release_stream(stream, peer.reuse_hash(), None);
        wait_for_drop(&stats).await;

        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert!(connector.reused_stream(&peer).await.is_none());
        assert_eq!(peer.identity_checks.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unexpected_idle_data_removes_stream_without_checkout() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(14);
        let (stream, stats) = instrumented_stream(104, TestRead::Data, None);

        connector.release_stream(stream, peer.reuse_hash(), None);
        wait_for_drop(&stats).await;

        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert!(connector.reused_stream(&peer).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn idle_read_error_removes_stream_without_checkout() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(15);
        let (stream, stats) = instrumented_stream(105, TestRead::Error, None);

        connector.release_stream(stream, peer.reuse_hash(), None);
        wait_for_drop(&stats).await;

        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert!(connector.reused_stream(&peer).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn idle_timeout_removes_pending_stream() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(16);
        let (stream, stats) = instrumented_stream(106, TestRead::Pending, None);

        connector.release_stream(
            stream,
            peer.reuse_hash(),
            Some(std::time::Duration::from_millis(20)),
        );
        wait_for_drop(&stats).await;

        assert_eq!(stats.reads.load(Ordering::SeqCst), 2);
        assert!(connector.reused_stream(&peer).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pool_capacity_evicts_lru_stream_and_keeps_new_stream() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let first_peer = PoolTestPeer::new(17);
        let second_peer = PoolTestPeer::new(18);
        let (first, first_stats) = instrumented_stream(107, TestRead::Pending, None);
        let (second, _second_stats) = instrumented_stream(108, TestRead::Pending, None);

        connector.release_stream(first, first_peer.reuse_hash(), None);
        first_stats.read.notified().await;
        connector.release_stream(second, second_peer.reuse_hash(), None);
        wait_for_drop(&first_stats).await;

        assert!(connector.reused_stream(&first_peer).await.is_none());
        assert_eq!(first_stats.reads.load(Ordering::SeqCst), 1);
        assert!(connector.reused_stream(&second_peer).await.is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn release_notifies_capacity_lifetime_once() {
        let connector = TransportConnector::new(Some(ConnectorOptions::new(1)));
        let peer = PoolTestPeer::new(19);
        let lifetime = Arc::new(TestLifetime::default());
        let (stream, stats) = instrumented_stream(
            109,
            TestRead::Pending,
            Some(lifetime.clone() as Arc<dyn ConnectionLifetime>),
        );

        connector.release_stream(stream, peer.reuse_hash(), None);

        assert_eq!(lifetime.reusable.load(Ordering::SeqCst), 1);
        assert!(connector.reused_stream(&peer).await.is_some());
        assert_eq!(stats.reads.load(Ordering::SeqCst), 1);
        assert_eq!(lifetime.reusable.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_connect() {
        let connector = TransportConnector::new(None);
        let peer = BasicPeer::new("1.1.1.1:80");
        // make a new connection to 1.1.1.1
        let stream = connector.new_stream(&peer).await.unwrap();
        connector.release_stream(stream, peer.reuse_hash(), None);

        let (_, reused) = connector.get_stream(&peer).await.unwrap();
        assert!(reused);
    }

    #[tokio::test]
    async fn test_connect_tls() {
        let connector = TransportConnector::new(None);
        let mut peer = BasicPeer::new("1.1.1.1:443");
        // BasicPeer will use tls when SNI is set
        peer.sni = "one.one.one.one".to_string();
        // make a new connection to https://1.1.1.1
        let stream = connector.new_stream(&peer).await.unwrap();
        connector.release_stream(stream, peer.reuse_hash(), None);

        #[cfg(unix)]
        crate::protocols::reset_peer_identity_syscalls();
        let (_, reused) = connector.get_stream(&peer).await.unwrap();
        assert!(reused);
        #[cfg(unix)]
        assert_eq!(crate::protocols::peer_identity_syscalls(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(unix)]
    async fn test_connect_uds() {
        let socket_path = test_utils::unique_uds_path("transport_connector");
        let (ready_rx, shutdown_tx, server_handle) =
            test_utils::spawn_mock_uds_server(socket_path.clone(), b"it works!");

        // Wait for the server to be ready before connecting
        ready_rx.await.unwrap();

        // create a new service at /tmp
        let connector = TransportConnector::new(None);
        let peer = BasicPeer::new_uds(&socket_path).unwrap();
        // make a new connection to mock uds
        let mut stream = connector.new_stream(&peer).await.unwrap();
        let mut buf = [0; 9];
        let _ = stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf, b"it works!");

        // Test connection reuse by releasing and getting the stream back
        let digest = stream.get_socket_digest().unwrap();
        assert_eq!(
            digest.peer_addr.get().and_then(Option::as_ref),
            Some(peer.address())
        );
        connector.release_stream(stream, peer.reuse_hash(), None);
        crate::protocols::reset_peer_identity_syscalls();
        let (stream, reused) = connector.get_stream(&peer).await.unwrap();
        assert!(reused);
        assert_eq!(crate::protocols::peer_identity_syscalls(), 0);

        // Clean up: drop the stream, tell server to shutdown, and wait for it
        drop(stream);
        let _ = shutdown_tx.send(());
        server_handle.await.unwrap();
    }

    async fn do_test_conn_timeout(conf: Option<ConnectorOptions>) {
        let connector = TransportConnector::new(conf);
        let mut peer = BasicPeer::new(BLACK_HOLE);
        peer.options.connection_timeout = Some(std::time::Duration::from_millis(1));
        let stream = connector.new_stream(&peer).await;
        match stream {
            Ok(_) => panic!("should throw an error"),
            Err(e) => assert_eq!(e.etype(), &ConnectTimedout),
        }
    }

    #[tokio::test]
    async fn test_conn_timeout() {
        do_test_conn_timeout(None).await;
    }

    #[tokio::test]
    async fn test_conn_timeout_with_offload() {
        let mut conf = ConnectorOptions::new(8);
        conf.offload_threadpool = Some((2, 2));
        do_test_conn_timeout(Some(conf)).await;
    }

    #[tokio::test]
    async fn test_connector_bind_to() {
        // connect to remote while bind to localhost will fail
        let peer = BasicPeer::new("240.0.0.1:80");
        let mut conf = ConnectorOptions::new(1);
        conf.bind_to_v4.push("127.0.0.1:0".parse().unwrap());
        let connector = TransportConnector::new(Some(conf));

        let stream = connector.new_stream(&peer).await;
        let error = stream.unwrap_err();
        // XXX: some systems will allow the socket to bind and connect without error, only to timeout
        assert!(error.etype() == &ConnectError || error.etype() == &ConnectTimedout)
    }

    /// Helper function for testing error handling in the `do_connect` function.
    /// This assumes that the connection will fail to on the peer and returns
    /// the decomposed error type and message
    async fn get_do_connect_failure_with_peer(peer: &BasicPeer) -> (ErrorType, String) {
        let tls_connector = Connector::new(None);
        let stream = do_connect(peer, None, None, &tls_connector.ctx).await;
        match stream {
            Ok(_) => panic!("should throw an error"),
            Err(e) => (
                e.etype().clone(),
                e.context
                    .as_ref()
                    .map(|ctx| ctx.as_str().to_owned())
                    .unwrap_or_default(),
            ),
        }
    }

    #[tokio::test]
    async fn test_do_connect_with_total_timeout() {
        let mut peer = BasicPeer::new(BLACK_HOLE);
        peer.options.total_connection_timeout = Some(std::time::Duration::from_millis(1));
        let (etype, context) = get_do_connect_failure_with_peer(&peer).await;
        assert_eq!(etype, ConnectTimedout);
        assert!(context.contains("total-connection timeout"));
    }

    #[tokio::test]
    async fn test_tls_connect_timeout_supersedes_total() {
        let mut peer = BasicPeer::new(BLACK_HOLE);
        peer.options.total_connection_timeout = Some(std::time::Duration::from_millis(10));
        peer.options.connection_timeout = Some(std::time::Duration::from_millis(1));
        let (etype, context) = get_do_connect_failure_with_peer(&peer).await;
        assert_eq!(etype, ConnectTimedout);
        assert!(!context.contains("total-connection timeout"));
    }

    #[tokio::test]
    async fn test_do_connect_without_total_timeout() {
        let peer = BasicPeer::new(BLACK_HOLE);
        let (etype, context) = get_do_connect_failure_with_peer(&peer).await;
        assert!(etype != ConnectTimedout || !context.contains("total-connection timeout"));
    }
}
