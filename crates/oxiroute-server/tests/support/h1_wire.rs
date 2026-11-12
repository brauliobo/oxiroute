#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! A real `oxiroute` process in front of one raw TCP origin: the wire tests of the HTTP/1.1 proxied path start the
//! binary with a plain listener and read exactly what the origin receives and what the client gets back.

use std::{
    net::SocketAddr,
    sync::{
        Arc, LockResult, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpPathSelector, HttpProxyPolicy,
    HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy, Listener, Protocol,
    UpstreamAlgorithm, UpstreamPool, UpstreamServer,
};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_rustls::TlsAcceptor;

use super::{
    config_support::{empty_config, socket_bind, socket_endpoint},
    fixture_support::{fixture, private_key_fixture},
    process_support::{ServerProcess, reserve_tcp_address},
};

pub const STEP: Duration = Duration::from_secs(5);

/// What a scripted origin does with one request.
#[derive(Clone, Copy, Debug)]
pub enum Step {
    Reply(&'static [u8]),
    /// Reads the request and closes the connection without a word.
    Drop,
    /// Answers after a while.
    Slow(Duration, &'static [u8]),
}

/// The requests an origin received, and how many are being received.
#[derive(Default)]
pub struct Capture {
    requests: Mutex<Vec<Vec<u8>>>,
    /// Requests of which bytes arrived and that are not recorded yet: a request is recorded when it is whole or when
    /// its connection ends, and the proxy ends the origin connection of a request cut short after the client side.
    receiving: AtomicUsize,
}

impl Capture {
    pub fn lock(&self) -> LockResult<MutexGuard<'_, Vec<Vec<u8>>>> {
        self.requests.lock()
    }

    /// Records a request that [`read_request_counted`] returned.
    fn record(&self, request: Vec<u8>) {
        self.lock().expect("captured requests").push(request);
        self.receiving.fetch_sub(1, Ordering::SeqCst);
    }
}

pub type Captured = Arc<Capture>;

/// An origin that answers every request with `reply` and records the request bytes it received.
pub struct Origin {
    pub address: SocketAddr,
    pub requests: Captured,
}

impl Origin {
    pub async fn start(reply: &'static [u8]) -> Self {
        Self::serve(reply, false).await
    }

    /// An origin that closes the connection after each reply.
    pub async fn start_closing(reply: &'static [u8]) -> Self {
        Self::serve(reply, true).await
    }

    /// An origin that speaks TLS (HTTP/1.1 only) with the `origin.example.test` fixture identity.
    pub async fn start_tls(reply: &'static [u8], closes: bool) -> Self {
        let certificates = CertificateDer::pem_file_iter(fixture("origin.pem"))
            .expect("origin certificate")
            .collect::<Result<Vec<_>, _>>()
            .expect("origin certificate chain");
        let key = private_key_fixture("origin-key.pem");
        let key = PrivateKeyDer::from_pem_file(key.path()).expect("origin key");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("origin protocol versions")
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .expect("origin identity");
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Self::serve_with(reply, closes, Some(TlsAcceptor::from(Arc::new(config)))).await
    }

    /// An origin that does `steps[n]` with the n-th request it receives, over all its connections (the last step
    /// for every request after the script): the replies and the failures a retry policy reacts to.
    pub async fn scripted(steps: &'static [Step]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let requests = Captured::default();
        let counter = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                let (seen, counter) = (Arc::clone(&seen), Arc::clone(&counter));
                tokio::spawn(async move {
                    while let Some(request) =
                        read_request_counted(&mut stream, &seen.receiving).await
                    {
                        seen.record(request);
                        let n = counter.fetch_add(1, Ordering::SeqCst);
                        match steps[n.min(steps.len() - 1)] {
                            Step::Reply(reply) => {
                                if stream.write_all(reply).await.is_err() {
                                    return;
                                }
                            }
                            Step::Drop => return,
                            Step::Slow(delay, reply) => {
                                tokio::time::sleep(delay).await;
                                if stream.write_all(reply).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        Self { address, requests }
    }

    async fn serve(reply: &'static [u8], closes: bool) -> Self {
        Self::serve_with(reply, closes, None).await
    }

    async fn serve_with(reply: &'static [u8], closes: bool, tls: Option<TlsAcceptor>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let requests = Captured::default();
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("origin accept");
                let (seen, tls) = (Arc::clone(&seen), tls.clone());
                tokio::spawn(async move {
                    match tls {
                        Some(acceptor) => {
                            if let Ok(stream) = acceptor.accept(stream).await {
                                answer(stream, reply, closes, seen).await;
                            }
                        }
                        None => answer(stream, reply, closes, seen).await,
                    }
                });
            }
        });
        Self { address, requests }
    }

    /// How many requests of which bytes arrived are not recorded yet.
    pub fn receiving(&self) -> usize {
        self.requests.receiving.load(Ordering::SeqCst)
    }

    /// Waits until every request of which bytes arrived is recorded: one cut short is recorded when the proxy ends
    /// the origin connection, which can be after the client connection ended.
    pub async fn settled(&self) {
        timeout(STEP, async {
            while self.receiving() != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the origin never recorded a request that was cut short");
    }

    pub fn requests(&self) -> Vec<String> {
        let requests = self.requests.lock().expect("captured requests");
        requests
            .iter()
            .map(|request| String::from_utf8_lossy(request).into_owned())
            .collect()
    }
}

async fn answer<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    reply: &'static [u8],
    closes: bool,
    seen: Captured,
) {
    while let Some(request) = read_request_counted(&mut stream, &seen.receiving).await {
        seen.record(request);
        if stream.write_all(reply).await.is_err() || closes {
            return;
        }
    }
}

/// The head of a request and the body its framing announces; `None` once the peer closed.
pub async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Option<Vec<u8>> {
    read_request_counted(stream, &AtomicUsize::new(0)).await
}

/// [`read_request`], with `receiving` one higher from the first byte of a request until it is recorded with
/// [`Capture::record`]; it is lowered here only when there is no request to record.
async fn read_request_counted<S: AsyncRead + Unpin>(
    stream: &mut S,
    receiving: &AtomicUsize,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let head_end = loop {
        if let Some(position) = find(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
        let count = stream.read(&mut buffer).await.unwrap_or(0);
        if count == 0 {
            receiving.fetch_sub(usize::from(!bytes.is_empty()), Ordering::SeqCst);
            return None;
        }
        if bytes.is_empty() {
            receiving.fetch_add(1, Ordering::SeqCst);
        }
        bytes.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok());
    let chunked = head.contains("transfer-encoding: chunked");
    loop {
        let complete = if chunked {
            bytes[head_end..].ends_with(b"0\r\n\r\n")
        } else {
            bytes.len() >= head_end + length.unwrap_or(0)
        };
        if complete {
            return Some(bytes);
        }
        let Ok(count) = stream.read(&mut buffer).await else {
            receiving.fetch_sub(1, Ordering::SeqCst);
            return None;
        };
        if count == 0 {
            return Some(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub fn config(listen: SocketAddr, origin: SocketAddr) -> ConfigDraft {
    let mut config = empty_config();
    config.listeners = vec![Listener {
        name: "wire".into(),
        bind: socket_bind(listen),
        protocol: Protocol::Http,
        service: Some("wire".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    }];
    config.upstream_pools = vec![UpstreamPool {
        name: "origin".into(),
        servers: vec![UpstreamServer {
            name: "origin".into(),
            endpoint: socket_endpoint(origin),
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }];
    config.http_services = vec![HttpService {
        drain_deadline_ms: None,
        name: "wire".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::Proxy {
                upstream_pool: "origin".into(),
                policy: HttpProxyPolicy::default().into(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 2_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    }];
    config
}

pub struct Wire {
    pub origin: Origin,
    pub proxy: SocketAddr,
    pub server: ServerProcess,
}

impl Wire {
    pub async fn start(reply: &'static [u8]) -> Self {
        Self::over(Origin::start(reply).await, |_| {}).await
    }

    /// The proxy in front of `origin`, with `edit` applied to the configuration first.
    pub async fn over(origin: Origin, edit: impl FnOnce(&mut ConfigDraft)) -> Self {
        Self::over_with_env(origin, &[], edit).await
    }

    /// [`Self::over`] with extra environment variables for the process (the internal test hooks).
    pub async fn over_with_env(
        origin: Origin,
        environment: &[(&str, &str)],
        edit: impl FnOnce(&mut ConfigDraft),
    ) -> Self {
        let proxy = reserve_tcp_address();
        let mut config = config(proxy, origin.address);
        edit(&mut config);
        let config = config.validate().expect("valid wire config");
        let mut server = ServerProcess::start_with_env(&config, None, environment);
        server.wait_for_tcp(proxy).await;
        Self {
            origin,
            proxy,
            server,
        }
    }

    /// Sends `request` on a new connection and returns everything the proxy sends until it closes
    /// the connection or goes quiet for a moment after the first bytes of the response. A process pays for what
    /// it sets up on its first request, so the first bytes are awaited for as long as a step may take.
    pub async fn exchange(&self, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(self.proxy).await.expect("connect");
        stream.write_all(request).await.expect("write request");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        timeout(STEP * 2, async {
            loop {
                let quiet = if bytes.is_empty() {
                    STEP
                } else {
                    Duration::from_millis(300)
                };
                match timeout(quiet, stream.read(&mut buffer)).await {
                    Ok(Ok(0) | Err(_)) | Err(_) => break,
                    Ok(Ok(count)) => bytes.extend_from_slice(&buffer[..count]),
                }
            }
        })
        .await
        .expect("response");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn finish(self) {
        self.server.shutdown();
    }
}

pub fn status(response: &str) -> &str {
    response.lines().next().unwrap_or_default()
}

pub fn header_names(head: &str) -> Vec<String> {
    head.split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.to_owned())
        .collect()
}
