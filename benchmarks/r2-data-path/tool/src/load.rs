//! Deterministic load generator: a fixed number of requests over a fixed set of connections.
//!
//! Protocols: `h1`, `h1tls`, `h2c`, `h2tls`, `h3` (QUIC), `ws` (HTTP upgrade then byte echo) and `tcp` (byte echo).
//! A warm-up phase opens the connections and sends `--warmup` requests; `--hook-start` runs to
//! completion, then exactly `--requests` requests are measured; `--hook-stop` runs afterwards.

use std::{
    io,
    net::SocketAddr,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, Uri,
    client::conn::http2::SendRequest,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::{
        Resumption,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::CryptoProvider,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

use crate::{Flags, flag, h3client};

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
type Io = Box<dyn Stream>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Proto {
    H1,
    H1Tls,
    H2c,
    H2Tls,
    H3,
    Ws,
    WsTls,
    Tcp,
    /// An HTTP/1.1 `CONNECT` tunnel to `--connect-to`, then raw messages through it.
    Connect,
}

impl std::str::FromStr for Proto {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        Ok(match value {
            "h1" => Self::H1,
            "h1tls" => Self::H1Tls,
            "h2c" => Self::H2c,
            "h2tls" => Self::H2Tls,
            "h3" => Self::H3,
            "ws" => Self::Ws,
            "wstls" => Self::WsTls,
            "tcp" => Self::Tcp,
            "connect" => Self::Connect,
            other => return Err(format!("unknown protocol {other}")),
        })
    }
}

struct Spec {
    proto: Proto,
    targets: Vec<SocketAddr>,
    host: String,
    conns: usize,
    streams: usize,
    requests: usize,
    warmup: usize,
    duration: Option<Duration>,
    warmup_duration: Option<Duration>,
    pace: Option<Duration>,
    /// Look for an idle connection the peer closed before each request (one extra read per request).
    reconnect_closed: bool,
    new_conn: bool,
    msg_bytes: usize,
    expect_status: u16,
    expect_len: Option<usize>,
    connector: Option<TlsConnector>,
    connect_to: String,
    /// One client endpoint (UDP socket) per connection, created inside the runtime (`h3` only).
    endpoints: Vec<quinn::Endpoint>,
    h1_request: Vec<u8>,
    /// Offset of an eight-digit counter in `h1_request` (`--vary`): every request carries a distinct query, so a
    /// cache sees a new key each time.
    vary_at: Option<usize>,
    /// A second request: fetched once on every new HTTP/1 connection before anything is measured (`--prime-path`), and
    /// in place of the main one on every `mix_every`-th request of a connection (`--mix-path`, `--mix-every`).
    h1_prime: Option<Vec<u8>>,
    h1_mix: Option<Vec<u8>>,
    mix_every: usize,
    mix_len: Option<usize>,
    h2_method: Method,
    h2_uri: Uri,
    h2_headers: HeaderMap,
    body: Bytes,
    checks: Checks,
}

#[derive(Default)]
struct Stats {
    latencies_us: Vec<u32>,
    connect_us: Vec<u32>,
    ok: u64,
    errors: u64,
    bytes: u64,
    connects: u64,
    /// Idle connections that the peer closed before the next request used them.
    reconnects: u64,
    first_error: Option<String>,
}

impl Stats {
    fn merge(&mut self, other: Self) {
        self.latencies_us.extend(other.latencies_us);
        self.connect_us.extend(other.connect_us);
        self.ok += other.ok;
        self.errors += other.errors;
        self.bytes += other.bytes;
        self.connects += other.connects;
        self.reconnects += other.reconnects;
        self.first_error = self.first_error.take().or(other.first_error);
    }
}

enum Client {
    H1(Io),
    H2(SendRequest<Full<Bytes>>),
    H3(h3client::Link),
    Raw(Io),
}

impl Client {
    /// The peer already closed an idle connection (an HTTP/2 connection after its GOAWAY): a client reconnects
    /// before sending instead of counting the request as failed, as every HTTP client does.
    fn peer_closed(&mut self) -> bool {
        match self {
            Self::H2(sender) => sender.is_closed(),
            Self::H3(link) => link.is_closed(),
            Self::H1(io) | Self::Raw(io) => {
                use futures_util::FutureExt;
                io.read(&mut [0u8; 1]).now_or_never().is_some()
            }
        }
    }
}

struct Outcome {
    status: u16,
    body_len: usize,
    close: bool,
    head_end: usize,
    /// The response met the `--expect-fill`, `--expect-header` and `--expect-no-header` checks.
    checks: bool,
}

impl Outcome {
    fn new(status: u16, body_len: usize, close: bool, head_end: usize) -> Self {
        Self { status, body_len, close, head_end, checks: true }
    }
}

/// Content checks beyond status and length: every body byte equals `fill`, a header has an exact value, a header is
/// absent. A proxy that answers with the right size but the wrong content, or ignores a header rule, counts as errors.
#[derive(Default)]
struct Checks {
    fill: Option<u8>,
    header: Option<(String, String)>,
    absent: Option<String>,
}

impl Checks {
    fn h1(&self, buf: &[u8], head_end: usize) -> bool {
        let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
        self.fill.is_none_or(|byte| buf[head_end..].iter().all(|&found| found == byte))
            && self.header.as_ref().is_none_or(|(name, value)| header_value(&head, &format!("{name}:")) == Some(value))
            && self.absent.as_ref().is_none_or(|name| header_value(&head, &format!("{name}:")).is_none())
    }

    fn h2(&self, headers: &HeaderMap, body: &[u8]) -> bool {
        self.fill.is_none_or(|byte| body.iter().all(|&found| found == byte))
            && self.header.as_ref().is_none_or(|(name, value)| {
                headers.get(name.as_str()).and_then(|found| found.to_str().ok()) == Some(value)
            })
            && self.absent.as_ref().is_none_or(|name| !headers.contains_key(name.as_str()))
    }
}

fn pad_headers(bytes: usize) -> Vec<(String, String)> {
    (0..bytes.div_ceil(100))
        .map(|index| (format!("x-pad-{index:02}"), "p".repeat(90)))
        .collect()
}

fn h1_request(
    method: &str,
    path: &str,
    host: &str,
    padding: &[(String, String)],
    body: &[u8],
    chunk_bytes: usize,
    keepalive: bool,
) -> Vec<u8> {
    let mut request = format!("{method} {path} HTTP/1.1\r\nhost: {host}\r\nuser-agent: r2tool\r\naccept: */*\r\n");
    for (name, value) in padding {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if chunk_bytes > 0 {
        request.push_str("transfer-encoding: chunked\r\n");
    } else if !body.is_empty() || method == "POST" {
        request.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    request.push_str(if keepalive {
        "connection: keep-alive\r\n\r\n"
    } else {
        "connection: close\r\n\r\n"
    });
    let mut bytes = request.into_bytes();
    if chunk_bytes > 0 {
        for chunk in body.chunks(chunk_bytes) {
            bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            bytes.extend_from_slice(chunk);
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(b"0\r\n\r\n");
    } else {
        bytes.extend_from_slice(body);
    }
    bytes
}

#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `browser` offers the post-quantum hybrid group first, with a share for X25519 next to it (what current browsers
/// send); any other value is the plain ring provider.
fn tls_provider(groups: &str) -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(match groups {
        "browser" => {
            let mut provider = rustls::crypto::aws_lc_rs::default_provider();
            provider.kx_groups = vec![
                rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768,
                rustls::crypto::aws_lc_rs::kx_group::X25519,
            ];
            provider
        }
        _ => rustls::crypto::ring::default_provider(),
    })
}

pub fn tls_config(alpn: &[u8], version: &str, resume: bool, groups: &str) -> ClientConfig {
    let provider = tls_provider(groups);
    let versions: &[&rustls::SupportedProtocolVersion] = match version {
        "1.2" => &[&rustls::version::TLS12],
        "1.3" => &[&rustls::version::TLS13],
        _ => &[&rustls::version::TLS13, &rustls::version::TLS12],
    };
    let mut config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(versions)
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    config.resumption = if resume {
        Resumption::in_memory_sessions(1024)
    } else {
        Resumption::disabled()
    };
    config
}

pub fn tls_connector(alpn: &[u8], version: &str, resume: bool, groups: &str) -> TlsConnector {
    TlsConnector::from(Arc::new(tls_config(alpn, version, resume, groups)))
}

fn seconds(value: &str, name: &str) -> Duration {
    Duration::from_secs_f64(value.parse().unwrap_or_else(|_| panic!("invalid value for --{name}")))
}

fn spec_from(flags: &Flags) -> Spec {
    let proto: Proto = flag(flags, "proto", Proto::H1);
    let host: String = flag(flags, "host", "bench.example.com".to_owned());
    let method: String = flag(flags, "method", "GET".to_owned());
    let path: String = flag(flags, "path", "/b1k".to_owned());
    let new_conn: bool = flag(flags, "new-conn", false);
    let mut padding = pad_headers(flag(flags, "hdr-bytes", 0));
    if let Some(header) = flags.get("extra-header") {
        let (name, value) = header.split_once(':').expect("--extra-header 'name: value'");
        padding.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let body = Bytes::from(vec![b'b'; flag(flags, "body-bytes", 0)]);
    let vary: bool = flag(flags, "vary", false);
    let path = if vary { format!("{path}?k=00000000") } else { path };
    let tls_version: String = flag(flags, "tls-version", "any".to_owned());
    let resume: bool = flag(flags, "resume", false);
    let tls_groups: String = flag(flags, "tls-groups", "default".to_owned());
    let connector = match proto {
        Proto::H1Tls | Proto::WsTls => Some(tls_connector(b"http/1.1", &tls_version, resume, &tls_groups)),
        Proto::H2Tls => Some(tls_connector(b"h2", &tls_version, resume, &tls_groups)),
        _ => None,
    };
    let mut h2_headers = HeaderMap::new();
    for (name, value) in &padding {
        h2_headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    let lower = |name: &str| flags.get(name).map(|value| value.to_ascii_lowercase());
    let checks = Checks {
        fill: flags.get("expect-fill").map(|value| value.bytes().next().expect("--expect-fill CHAR")),
        header: lower("expect-header").map(|header| {
            let (name, value) = header.split_once(':').expect("--expect-header 'name: value'");
            (name.trim().to_owned(), value.trim().to_owned())
        }),
        absent: lower("expect-no-header"),
    };
    let scheme = if matches!(proto, Proto::H2Tls | Proto::H3) { "https" } else { "http" };
    Spec {
        proto,
        targets: flag(flags, "target", "127.0.0.1:0".to_owned())
            .split(',')
            .map(|address| address.parse().expect("--target address list"))
            .collect(),
        conns: flag(flags, "conns", 1),
        streams: flag(flags, "streams", 1),
        requests: flag(flags, "requests", 1000),
        warmup: flag(flags, "warmup", 200),
        duration: flags.get("duration").map(|value| seconds(value, "duration")),
        warmup_duration: flags.get("warmup-duration").map(|value| seconds(value, "warmup-duration")),
        reconnect_closed: flag(flags, "reconnect-closed", false),
        pace: flags.get("pace-ms").map(|value| seconds(value, "pace-ms") / 1000),
        new_conn,
        msg_bytes: flag(flags, "msg-bytes", 1024),
        expect_status: flag(flags, "expect-status", 200),
        expect_len: flags
            .get("expect-len")
            .map(|value| value.parse().expect("--expect-len")),
        connector,
        endpoints: Vec::new(),
        h1_request: h1_request(&method, &path, &host, &padding, &body, flag(flags, "chunk-bytes", 0), !new_conn),
        connect_to: flag(flags, "connect-to", String::new()),
        vary_at: vary.then(|| method.len() + 1 + path.len() - 8),
        h1_prime: flags
            .get("prime-path")
            .map(|prime| h1_request("GET", prime, &host, &[], &[], 0, true)),
        h1_mix: flags
            .get("mix-path")
            .map(|mix| h1_request("GET", mix, &host, &[], &[], 0, true)),
        mix_every: flag(flags, "mix-every", 0),
        mix_len: flags.get("mix-len").map(|value| value.parse().expect("--mix-len")),
        h2_method: method.parse().expect("method"),
        h2_uri: format!("{scheme}://{host}{path}").parse().expect("uri"),
        h2_headers,
        host,
        body,
        checks,
    }
}

async fn connect(spec: &Spec, slot: usize) -> io::Result<Client> {
    if spec.proto == Proto::H3 {
        let target = spec.targets[slot % spec.targets.len()];
        let endpoint = &spec.endpoints[slot % spec.endpoints.len()];
        return h3client::Link::connect(endpoint, target, &spec.host).await.map(Client::H3);
    }
    let tcp = TcpStream::connect(spec.targets[slot % spec.targets.len()]).await?;
    tcp.set_nodelay(true)?;
    let mut io: Io = match &spec.connector {
        Some(connector) => {
            let name = ServerName::try_from(spec.host.clone()).map_err(io::Error::other)?;
            Box::new(connector.connect(name, tcp).await?)
        }
        None => Box::new(tcp),
    };
    match spec.proto {
        Proto::H1 | Proto::H1Tls => {
            if let Some(prime) = &spec.h1_prime {
                io.write_all(prime).await?;
                let outcome = read_response(&mut io, &mut Vec::new()).await?;
                if outcome.status != 200 {
                    return Err(io::Error::other(format!("prime request answered {}", outcome.status)));
                }
            }
            Ok(Client::H1(io))
        }
        Proto::H3 => unreachable!("QUIC clients return before the TCP connect"),
        Proto::Tcp => Ok(Client::Raw(io)),
        Proto::Connect => {
            let destination = &spec.connect_to;
            let request = format!("CONNECT {destination} HTTP/1.1\r\nhost: {destination}\r\n\r\n");
            io.write_all(request.as_bytes()).await?;
            let mut head = Vec::with_capacity(256);
            while find(&head, b"\r\n\r\n").is_none() {
                if io.read_buf(&mut head).await? == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
            }
            if !head.starts_with(b"HTTP/1.1 200") {
                return Err(io::Error::other(format!("CONNECT answered {}", String::from_utf8_lossy(&head))));
            }
            Ok(Client::Raw(io))
        }
        Proto::H2c | Proto::H2Tls => {
            let (sender, connection) =
                hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(io))
                    .await
                    .map_err(io::Error::other)?;
            tokio::spawn(connection);
            Ok(Client::H2(sender))
        }
        Proto::Ws | Proto::WsTls => {
            let handshake = format!(
                "GET /ws HTTP/1.1\r\nhost: {}\r\nconnection: upgrade\r\nupgrade: websocket\r\n\
                 sec-websocket-version: 13\r\nsec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                spec.host
            );
            io.write_all(handshake.as_bytes()).await?;
            let mut scratch = Vec::with_capacity(1024);
            let outcome = read_response(&mut io, &mut scratch).await?;
            if outcome.status != 101 {
                return Err(io::Error::other(format!("upgrade answered {}", outcome.status)));
            }
            Ok(Client::Raw(io))
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .find_map(|line| line.strip_prefix(name))
        .map(str::trim)
}

async fn read_response(io: &mut Io, buf: &mut Vec<u8>) -> io::Result<Outcome> {
    buf.clear();
    let head_end = loop {
        if let Some(position) = find(buf, b"\r\n\r\n") {
            break position + 4;
        }
        if io.read_buf(buf).await? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| io::Error::other("malformed status line"))?;
    let mut close = header_value(&head, "connection:").is_some_and(|value| value.contains("close"));
    if status < 200 || status == 204 || status == 304 {
        return Ok(Outcome::new(status, 0, close, head_end));
    }
    if header_value(&head, "transfer-encoding:").is_some_and(|value| value.contains("chunked")) {
        while !buf[head_end..].ends_with(b"0\r\n\r\n") {
            if io.read_buf(buf).await? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        return Ok(Outcome::new(status, buf.len() - head_end, close, head_end));
    }
    if let Some(length) = header_value(&head, "content-length:").and_then(|value| value.parse::<usize>().ok()) {
        while buf.len() < head_end + length {
            if io.read_buf(buf).await? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        return Ok(Outcome::new(status, length, close, head_end));
    }
    close = true;
    while io.read_buf(buf).await? != 0 {}
    Ok(Outcome::new(status, buf.len() - head_end, close, head_end))
}

fn request_for<B>(spec: &Spec, body: B) -> Request<B> {
    let mut request = Request::new(body);
    *request.method_mut() = spec.h2_method.clone();
    *request.uri_mut() = spec.h2_uri.clone();
    *request.headers_mut() = spec.h2_headers.clone();
    request
}

async fn exchange(client: &mut Client, spec: &Spec, scratch: &mut Vec<u8>, request: &[u8]) -> io::Result<Outcome> {
    scratch.clear();
    match client {
        Client::H1(io) => {
            match spec.vary_at {
                Some(at) if std::ptr::eq(request.as_ptr(), spec.h1_request.as_ptr()) => {
                    static VARY: AtomicUsize = AtomicUsize::new(0);
                    let mut request = request.to_vec();
                    let counter = format!("{:08}", VARY.fetch_add(1, Ordering::Relaxed) % 100_000_000);
                    request[at..at + 8].copy_from_slice(counter.as_bytes());
                    io.write_all(&request).await?;
                }
                _ => io.write_all(request).await?,
            }
            let mut outcome = read_response(io, scratch).await?;
            outcome.checks = spec.checks.h1(scratch, outcome.head_end);
            Ok(outcome)
        }
        Client::H2(sender) => {
            let request = request_for(spec, Full::new(spec.body.clone()));
            sender.ready().await.map_err(io::Error::other)?;
            let response = sender.send_request(request).await.map_err(io::Error::other)?;
            let (parts, body) = response.into_parts();
            let body = body.collect().await.map_err(io::Error::other)?.to_bytes();
            let mut outcome = Outcome::new(parts.status.as_u16(), body.len(), false, 0);
            outcome.checks = spec.checks.h2(&parts.headers, &body);
            Ok(outcome)
        }
        Client::H3(link) => {
            let (status, body_len) = link.request(request_for(spec, ()), &spec.body).await?;
            Ok(Outcome::new(status, body_len, false, 0))
        }
        Client::Raw(io) => {
            scratch.clear();
            scratch.resize(spec.msg_bytes, b'm');
            io.write_all(scratch).await?;
            io.read_exact(scratch).await?;
            Ok(Outcome::new(200, spec.msg_bytes, false, 0))
        }
    }
}

fn micros(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// A request that failed on a connection the client had used before, before any byte of an answer arrived, or that a
/// server refused with a graceful GOAWAY (or left unanswered by the close that follows it): the server closed the connection while the request was on its way (idle
/// close at a reload, keep-alive limit), the request was not processed, and a client sends it again on a new
/// connection. Counted as `reconnects`, never as a failure.
fn stale_connection(error: &io::Error, text: &str, scratch: &[u8]) -> bool {
    let closed = matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
    );
    // HTTP/2: refused by a graceful GOAWAY, or still unanswered when the connection closed after it.
    let goaway = (text.contains("GoAway(") && text.contains("NO_ERROR")) || text.contains("Canceled, \"connection closed\"");
    (closed && scratch.is_empty()) || goaway
}

async fn worker(
    spec: Arc<Spec>,
    slot: usize,
    mut client: Option<Client>,
    next: Arc<AtomicUsize>,
    total: usize,
    deadline: Option<Instant>,
    record: bool,
) -> (Option<Client>, Stats) {
    let mut stats = Stats::default();
    let mut scratch = Vec::with_capacity(64 * 1024);
    let mut retrying = false;
    let mut sent = 0_usize;
    while retrying || (next.fetch_add(1, Ordering::Relaxed) < total && deadline.is_none_or(|limit| Instant::now() < limit)) {
        let retry = std::mem::take(&mut retrying);
        let reused = client.is_some();
        if spec.reconnect_closed && client.as_mut().is_some_and(Client::peer_closed) {
            client = None;
            stats.reconnects += 1;
        }
        let started = Instant::now();
        if client.is_none() {
            let connecting = Instant::now();
            match connect(&spec, slot).await {
                Ok(connected) => {
                    stats.connects += 1;
                    if record {
                        stats.connect_us.push(micros(connecting));
                    }
                    client = Some(connected);
                }
                Err(_) => {
                    stats.errors += 1;
                    continue;
                }
            }
        }
        sent += 1;
        let mixed = spec.mix_every > 0 && sent % spec.mix_every == 0;
        let (request, expected) = match (&spec.h1_mix, mixed) {
            (Some(mix), true) => (mix.as_slice(), spec.mix_len),
            _ => (spec.h1_request.as_slice(), spec.expect_len),
        };
        let outcome = exchange(client.as_mut().expect("connected"), &spec, &mut scratch, request).await;
        match outcome {
            Ok(outcome) => {
                let valid = outcome.status == spec.expect_status
                    && outcome.checks
                    && expected.is_none_or(|length| length == outcome.body_len);
                if valid {
                    stats.ok += 1;
                    stats.bytes += outcome.body_len as u64;
                    if record {
                        stats.latencies_us.push(micros(started));
                    }
                } else {
                    stats.errors += 1;
                }
                if outcome.close || spec.new_conn {
                    client = None;
                }
            }
            Err(error) => {
                let text = format!("{error:?}");
                if spec.reconnect_closed && reused && !retry && stale_connection(&error, &text, &scratch) {
                    stats.reconnects += 1;
                    retrying = true;
                } else {
                    stats.errors += 1;
                    stats.first_error.get_or_insert(text);
                }
                client = None;
            }
        }
        if let Some(pace) = spec.pace {
            tokio::time::sleep(pace).await;
        }
    }
    (client, stats)
}

/// A phase ends after `total` requests, or at `duration` when one is given (the count is then ignored).
async fn phase(
    spec: &Arc<Spec>,
    clients: &mut Vec<Option<Client>>,
    total: usize,
    duration: Option<Duration>,
    record: bool,
) -> Stats {
    let next = Arc::new(AtomicUsize::new(0));
    let total = if duration.is_some() { usize::MAX } else { total };
    let deadline = duration.map(|length| Instant::now() + length);
    let handles: Vec<_> = clients
        .drain(..)
        .enumerate()
        .map(|(slot, client)| tokio::spawn(worker(Arc::clone(spec), slot, client, Arc::clone(&next), total, deadline, record)))
        .collect();
    let mut merged = Stats::default();
    for handle in handles {
        let (client, stats) = handle.await.expect("worker task");
        clients.push(client);
        merged.merge(stats);
    }
    merged
}

async fn initial_clients(spec: &Spec) -> io::Result<Vec<Option<Client>>> {
    let mut clients = Vec::with_capacity(spec.conns * spec.streams);
    for slot in 0..spec.conns {
        if spec.new_conn {
            clients.extend((0..spec.streams).map(|_| None));
            continue;
        }
        match connect(spec, slot).await? {
            Client::H2(sender) => {
                clients.extend((0..spec.streams).map(|_| Some(Client::H2(sender.clone()))));
            }
            Client::H3(link) => {
                clients.extend((0..spec.streams).map(|_| Some(Client::H3(link.clone()))));
            }
            other => clients.push(Some(other)),
        }
    }
    Ok(clients)
}

async fn hook(command: Option<&String>) {
    if let Some(command) = command {
        let running = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .kill_on_drop(true)
            .status();
        let status = tokio::time::timeout(std::time::Duration::from_secs(60), running)
            .await
            .unwrap_or_else(|_| panic!("hook timed out: {command}"))
            .expect("spawn hook");
        assert!(status.success(), "hook failed: {command}");
    }
}

fn percentile(sorted: &[u32], fraction: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

async fn drive(spec: Arc<Spec>, flags: &Flags) -> ExitCode {
    let mut clients = match initial_clients(&spec).await {
        Ok(clients) => clients,
        Err(error) => {
            eprintln!("connect failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let warm = phase(&spec, &mut clients, spec.warmup, spec.warmup_duration, false).await;
    hook(flags.get("hook-start")).await;
    let started = Instant::now();
    let mut stats = phase(&spec, &mut clients, spec.requests, spec.duration, true).await;
    let elapsed = started.elapsed();
    hook(flags.get("hook-stop")).await;
    stats.latencies_us.sort_unstable();
    stats.connect_us.sort_unstable();
    let seconds = elapsed.as_secs_f64();
    println!("requests={}", stats.ok + stats.errors);
    println!("ok={}", stats.ok);
    println!("errors={}", stats.errors + warm.errors);
    println!("elapsed_ms={:.1}", seconds * 1000.0);
    println!("rps={:.0}", stats.ok as f64 / seconds);
    println!("mbps={:.1}", stats.bytes as f64 * 8.0 / seconds / 1e6);
    for (label, fraction) in [("p50", 0.5), ("p99", 0.99), ("p999", 0.999), ("max", 1.0)] {
        println!("{label}_us={}", percentile(&stats.latencies_us, fraction));
    }
    println!("connects={}", stats.connects);
    println!("reconnects={}", stats.reconnects);
    if let Some(error) = &stats.first_error {
        println!("first_error={error}");
    }
    println!("connect_p50_us={}", percentile(&stats.connect_us, 0.5));
    println!("connect_p99_us={}", percentile(&stats.connect_us, 0.99));
    if stats.errors + warm.errors == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

pub fn run(flags: &Flags) -> ExitCode {
    let mut spec = spec_from(flags);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(flag(flags, "threads", 2))
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        if spec.proto == Proto::H3 {
            let tls = || h3client::endpoint(tls_config(b"h3", "1.3", false, "default")).expect("quic client endpoint");
            spec.endpoints = (0..spec.conns).map(|_| tls()).collect();
        }
        drive(Arc::new(spec), flags).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nX-Served-By: bench\r\ncontent-length: 4\r\n\r\nxxxx";
    const HEAD_END: usize = RESPONSE.len() - 4;

    fn checks(fill: Option<u8>, header: Option<(&str, &str)>, absent: Option<&str>) -> Checks {
        Checks {
            fill,
            header: header.map(|(name, value)| (name.to_owned(), value.to_owned())),
            absent: absent.map(str::to_owned),
        }
    }

    #[test]
    fn h1_checks_pass_and_fail() {
        assert!(checks(Some(b'x'), Some(("x-served-by", "bench")), Some("content-type")).h1(RESPONSE, HEAD_END));
        assert!(!checks(Some(b'y'), None, None).h1(RESPONSE, HEAD_END));
        assert!(!checks(None, Some(("x-served-by", "other")), None).h1(RESPONSE, HEAD_END));
        assert!(!checks(None, Some(("x-missing", "bench")), None).h1(RESPONSE, HEAD_END));
        assert!(!checks(None, None, Some("content-length")).h1(RESPONSE, HEAD_END));
    }

    #[test]
    fn h2_checks_pass_and_fail() {
        let mut headers = HeaderMap::new();
        headers.insert("x-served-by", HeaderValue::from_static("bench"));
        assert!(checks(Some(b'x'), Some(("x-served-by", "bench")), Some("content-type")).h2(&headers, b"xxxx"));
        assert!(!checks(Some(b'x'), None, None).h2(&headers, b"xxyx"));
        assert!(!checks(None, Some(("x-served-by", "other")), None).h2(&headers, b""));
        assert!(!checks(None, None, Some("x-served-by")).h2(&headers, b""));
    }
}
