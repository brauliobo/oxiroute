//! Reverse HTTP/3 routes reach HTTP/1.1 and HTTP/2 origins through the pools the other listeners use.
//!
//! The helpers: a scripted HTTP/1.1 origin (TCP or Unix socket), an HTTP/3 client on a real daemon and the
//! configuration of a reverse listener whose routes proxy to the given pools.
#[allow(clippy::wildcard_imports)]
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite},
    net::{TcpListener, UnixListener},
    task::JoinHandle,
};

mod differential;
mod failures;
mod plain;
mod policy;
mod secure;

pub(super) const AUTHORITY: &str = "proxy.example.test";

/// A request as the scripted origin received it; header names are lower case.
#[derive(Clone, Debug)]
pub(super) struct Seen {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl Seen {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        let mut values = self.headers.iter().filter(|(key, _)| key == name);
        values.next().map(|(_, value)| value.as_str())
    }
}

/// What the scripted origin does with a request.
pub(super) enum Reply {
    /// Writes the bytes and waits for the next request.
    Send(Vec<u8>),
    /// Writes the bytes and closes the connection.
    SendAndClose(Vec<u8>),
    /// Writes the bytes after a pause, unless the peer closes the connection first.
    After(Duration, Vec<u8>),
}

type Script = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

pub(super) fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\n").into_bytes();
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    let bodiless =
        status.starts_with('1') || status.starts_with("204") || status.starts_with("304");
    if !bodiless
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    {
        out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

pub(super) struct ScriptedOrigin {
    pub(super) seen: Arc<Mutex<Vec<Seen>>>,
    pub(super) connections: Arc<AtomicUsize>,
    pub(super) aborted: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl ScriptedOrigin {
    pub(super) async fn tcp(
        script: impl Fn(&Seen) -> Reply + Send + Sync + 'static,
    ) -> (Self, SocketAddr) {
        let listener = Arc::new(
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("origin bind"),
        );
        let address = listener.local_addr().expect("origin address");
        let origin = Self::spawn(script, move || {
            let listener = Arc::clone(&listener);
            async move { listener.accept().await.map(|(stream, _)| stream) }
        });
        (origin, address)
    }

    pub(super) fn unix(
        path: &Path,
        script: impl Fn(&Seen) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = Arc::new(UnixListener::bind(path).expect("origin socket"));
        Self::spawn(script, move || {
            let listener = Arc::clone(&listener);
            async move { listener.accept().await.map(|(stream, _)| stream) }
        })
    }

    fn spawn<S, F, Fut>(script: impl Fn(&Seen) -> Reply + Send + Sync + 'static, accept: F) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = std::io::Result<S>> + Send,
    {
        let script: Script = Arc::new(script);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let aborted = Arc::new(AtomicUsize::new(0));
        let state = (
            Arc::clone(&seen),
            Arc::clone(&connections),
            Arc::clone(&aborted),
        );
        let task = tokio::spawn(async move {
            while let Ok(stream) = accept().await {
                state.1.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serve(
                    stream,
                    Arc::clone(&script),
                    Arc::clone(&state.0),
                    Arc::clone(&state.2),
                ));
            }
        });
        Self {
            seen,
            connections,
            aborted,
            task,
        }
    }

    pub(super) fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen requests").clone()
    }

    pub(super) fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for ScriptedOrigin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve<S>(
    mut stream: S,
    script: Script,
    seen: Arc<Mutex<Vec<Seen>>>,
    aborted: Arc<AtomicUsize>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(request) = read_request(&mut stream).await {
        let reply = script(&request);
        seen.lock().expect("seen requests").push(request);
        let ok = match reply {
            Reply::Send(bytes) => stream.write_all(&bytes).await.is_ok(),
            Reply::SendAndClose(bytes) => {
                let _ = stream.write_all(&bytes).await;
                return;
            }
            Reply::After(pause, bytes) => {
                let mut probe = [0_u8; 1];
                tokio::select! {
                    () = sleep(pause) => stream.write_all(&bytes).await.is_ok(),
                    _ = stream.read(&mut probe) => {
                        aborted.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                }
            }
        };
        if !ok {
            return;
        }
    }
}

async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Option<Seen> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end - 4]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<Vec<_>>();
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map_or(0, |(_, value)| value.parse().expect("request length"));
    while buffer.len() < head_end + length {
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
        }
    }
    Some(Seen {
        method,
        target,
        headers,
        body: buffer[head_end..head_end + length].to_vec(),
    })
}

/// What an HTTP/3 client saw.
#[derive(Debug)]
pub(super) struct Answer {
    pub(super) status: u16,
    pub(super) headers: HeaderMap,
    pub(super) body: Bytes,
    pub(super) trailers: Option<HeaderMap>,
}

impl Answer {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(name)
            .map(|value| value.to_str().expect("text header"))
    }

    pub(super) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A daemon with a reverse HTTP/3 listener and an HTTP/3 client connected to it.
pub(super) struct Daemon {
    pub(super) server: process_support::ServerProcess,
    endpoint: quinn::Endpoint,
    sender: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    driver: JoinHandle<()>,
}

impl Daemon {
    pub(super) async fn start(
        listener: SocketAddr,
        config: &oxiroute_config::ValidatedConfig,
    ) -> Self {
        let server = process_support::ServerProcess::start(config, None);
        let endpoint = client_endpoint().expect("H3 client endpoint");
        let connection = connect_h3(&endpoint, listener).await;
        let (driver, sender) = h3::client::new(h3_quinn::Connection::new(connection))
            .await
            .expect("H3 client connection");
        Self {
            server,
            endpoint,
            sender,
            driver: drive_client(driver),
        }
    }

    pub(super) async fn send(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Answer {
        let mut stream = self.open(method, path, headers, body).await;
        let response = stream.recv_response().await.expect("H3 response");
        let received = recv_body(&mut stream).await;
        Answer {
            status: response.status().as_u16(),
            headers: response.headers().clone(),
            body: received,
            trailers: stream.recv_trailers().await.expect("H3 response trailers"),
        }
    }

    pub(super) async fn get(&mut self, path: &str) -> Answer {
        self.send("GET", path, &[], b"").await
    }

    /// Sends the request and returns its stream before the response is read.
    pub(super) async fn open(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes> {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("https://{AUTHORITY}{path}"));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut stream = self
            .sender
            .send_request(request.body(()).expect("H3 request"))
            .await
            .expect("send H3 request");
        if !body.is_empty() {
            stream
                .send_data(Bytes::copy_from_slice(body))
                .await
                .expect("H3 request body");
        }
        stream.finish().await.expect("finish H3 request");
        stream
    }

    pub(super) async fn finish(self) {
        drop(self.sender);
        self.endpoint
            .close(quinn::VarInt::from_u32(0), b"test complete");
        self.driver.await.expect("H3 driver task");
        self.server.shutdown_gracefully();
    }
}

pub(super) fn pool(name: &str, endpoints: Vec<oxiroute_config::UpstreamEndpoint>) -> UpstreamPool {
    let mut pool = h3_pool(SocketAddr::from((Ipv4Addr::LOCALHOST, 1)));
    pool.name = name.into();
    pool.endpoints = endpoints;
    pool.tls = None;
    pool.http_versions = HttpVersionPolicy {
        min: HttpVersion::Http11,
        max: HttpVersion::Http11,
    };
    pool
}

pub(super) fn tcp_pool(name: &str, addresses: &[SocketAddr]) -> UpstreamPool {
    pool(
        name,
        addresses
            .iter()
            .map(|address| support::socket_endpoint(*address))
            .collect(),
    )
}

/// A pool of TLS origins that speak the given versions.
pub(super) fn tls_pool(
    name: &str,
    address: SocketAddr,
    min: HttpVersion,
    max: HttpVersion,
) -> UpstreamPool {
    let mut pool = h3_pool(address);
    pool.name = name.into();
    pool.http_versions = HttpVersionPolicy { min, max };
    pool
}

pub(super) fn route(path: &str, pool: &str, policy: HttpProxyPolicy) -> HttpRoute {
    let mut route = proxy_service(64 * 1024).routes.remove(0);
    route.path = HttpPathSelector::SegmentPrefix { value: path.into() };
    route.action = HttpRouteAction::Proxy {
        upstream_pool: pool.into(),
        policy: policy.into(),
    };
    route
}

pub(super) fn service(routes: Vec<HttpRoute>) -> HttpService {
    let mut service = proxy_service(64 * 1024);
    service.routes = routes;
    service
}

pub(super) fn draft(
    listener: SocketAddr,
    service: HttpService,
    pools: Vec<UpstreamPool>,
) -> ConfigDraft {
    static KEY: std::sync::OnceLock<fixture_support::PrivateKeyFixture> =
        std::sync::OnceLock::new();
    let key = KEY.get_or_init(|| fixture_support::private_key_fixture("proxy-a-key.pem"));
    reverse_config(listener, key.path(), service, pools, Some(8))
}

pub(super) fn config(
    listener: SocketAddr,
    service: HttpService,
    pools: Vec<UpstreamPool>,
) -> oxiroute_config::ValidatedConfig {
    draft(listener, service, pools)
        .validate()
        .expect("valid reverse HTTP/3 configuration")
}

/// A daemon whose one route proxies everything to the pool built from `addresses`.
pub(super) async fn daemon_for(addresses: &[SocketAddr], policy: HttpProxyPolicy) -> Daemon {
    let listener = reserve_udp_address();
    let config = config(
        listener,
        service(vec![route("/", "origin", policy)]),
        vec![tcp_pool("origin", addresses)],
    );
    Daemon::start(listener, &config).await
}

pub(super) fn ok(body: &[u8]) -> Reply {
    Reply::Send(response("200 OK", &[("Content-Type", "text/plain")], body))
}
