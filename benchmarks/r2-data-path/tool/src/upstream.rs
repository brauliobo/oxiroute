//! Fixed-body HTTP/1.1 + h2c upstream and a raw TCP echo.
//!
//! Paths: `/b0 /b1k /b64k /b1m` fixed `application/octet-stream` bodies, `/html64k` compressible
//! `text/html`, `/stream` chunked 64 KiB without delay, `/slow` chunked 64 KiB with a 2 ms gap between
//! chunks, `/sink` (any method; drains the request body, answers `ok`), `/ws` (101 then byte echo),
//! `/echo-headers` (the request headers as text, one per line), `/b1k-slow` like `/b1k` with one response in 500 delayed 200 ms.

use std::{
    convert::Infallible,
    io,
    net::SocketAddr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Frame, Incoming},
    header::{CONNECTION, CONTENT_TYPE, HeaderValue, UPGRADE},
    service::service_fn,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
};
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio_rustls::TlsAcceptor;

use crate::{Flags, flag};

type Body = BoxBody<Bytes, Infallible>;

const CHUNK: usize = 4096;
const CHUNKS: usize = 16;

struct Payloads {
    b1k: Bytes,
    b64k: Bytes,
    b1m: Bytes,
    html64k: Bytes,
    chunk: Bytes,
}

fn payloads() -> &'static Payloads {
    static PAYLOADS: OnceLock<Payloads> = OnceLock::new();
    PAYLOADS.get_or_init(|| {
        let octets = |size: usize| Bytes::from(vec![b'x'; size]);
        let html = "<p>the quick brown fox jumps over the lazy dog</p>\n".repeat(2000);
        Payloads {
            b1k: octets(1024),
            b64k: octets(64 * 1024),
            b1m: octets(1024 * 1024),
            html64k: Bytes::from(html.into_bytes()).slice(..64 * 1024),
            chunk: octets(CHUNK),
        }
    })
}

fn full(content_type: &'static str, body: Bytes) -> Response<Body> {
    let mut response = Response::new(Full::new(body).boxed());
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn chunked(gap: Option<Duration>) -> Response<Body> {
    let chunk = payloads().chunk.clone();
    let frames = stream::unfold(0, move |sent| {
        let chunk = chunk.clone();
        async move {
            if sent == CHUNKS {
                return None;
            }
            if let (Some(gap), true) = (gap, sent > 0) {
                tokio::time::sleep(gap).await;
            }
            Some((Ok::<_, Infallible>(Frame::data(chunk)), sent + 1))
        }
    });
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(frames)));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
}

async fn echo_upgraded(upgraded: hyper::upgrade::Upgraded) {
    let (mut reader, mut writer) = tokio::io::split(TokioIo::new(upgraded));
    let _ = tokio::io::copy(&mut reader, &mut writer).await;
}

/// `/b1k-slow` answers like `/b1k`, but one request in `SLOW_EVERY` waits `SLOW_DELAY` first (slow-handler lanes).
const SLOW_EVERY: u64 = 500;
const SLOW_DELAY: Duration = Duration::from_millis(200);
static SLOW_COUNTER: AtomicU64 = AtomicU64::new(0);

async fn handle(mut request: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let payloads = payloads();
    let path = request.uri().path().to_owned();
    Ok(match path.as_str() {
        "/b0" => full("application/octet-stream", Bytes::new()),
        "/b1k" => full("application/octet-stream", payloads.b1k.clone()),
        "/b1k-slow" => {
            if SLOW_COUNTER.fetch_add(1, Ordering::Relaxed) % SLOW_EVERY == 0 {
                tokio::time::sleep(SLOW_DELAY).await;
            }
            full("application/octet-stream", payloads.b1k.clone())
        }
        "/b64k" => full("application/octet-stream", payloads.b64k.clone()),
        "/b1m" => full("application/octet-stream", payloads.b1m.clone()),
        "/html64k" => full("text/html", payloads.html64k.clone()),
        "/stream" => chunked(None),
        "/slow" => chunked(Some(Duration::from_millis(2))),
        "/echo-headers" => {
            let mut text = String::new();
            for (name, value) in request.headers() {
                text.push_str(&format!("{name}: {}\n", value.to_str().unwrap_or("?")));
            }
            full("text/plain", Bytes::from(text))
        }
        "/sink" => {
            let _ = request.into_body().collect().await;
            full("text/plain", Bytes::from_static(b"ok"))
        }
        "/ws" => {
            let upgrade = hyper::upgrade::on(&mut request);
            tokio::spawn(async move {
                if let Ok(upgraded) = upgrade.await {
                    echo_upgraded(upgraded).await;
                }
            });
            let mut response = Response::new(Full::new(Bytes::new()).boxed());
            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            response
                .headers_mut()
                .insert(CONNECTION, HeaderValue::from_static("upgrade"));
            response
                .headers_mut()
                .insert(UPGRADE, HeaderValue::from_static("websocket"));
            // The accept value of the fixed key `r2tool load` sends (RFC 6455 section 1.3); HAProxy rejects a 101 without it.
            response.headers_mut().insert(
                "sec-websocket-accept",
                HeaderValue::from_static("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            );
            response
        }
        _ => {
            let mut response = full("text/plain", Bytes::from_static(b"not found\n"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
    })
}

/// Deep accept queue so connection storms (4096 connections) never lose a SYN to a full backlog.
fn bind_deep(address: SocketAddr) -> TcpListener {
    let socket = TcpSocket::new_v4().expect("socket");
    socket.set_reuseaddr(true).expect("reuseaddr");
    socket.bind(address).expect("bind");
    socket.listen(65535).expect("listen")
}

/// TLS for the HTTP listeners (`--tls-cert` and `--tls-key`, PEM files): the upstream then offers `h2` and
/// `http/1.1`, which is what an `http_versions` pool of `2` needs.
fn tls_acceptor(flags: &Flags) -> Option<TlsAcceptor> {
    let cert: String = flags.get("tls-cert")?.clone();
    let key: String = flag(flags, "tls-key", String::new());
    let certs = CertificateDer::pem_file_iter(&cert)
        .expect("--tls-cert")
        .collect::<Result<Vec<_>, _>>()
        .expect("certificate pem");
    let key = PrivateKeyDer::from_pem_file(&key).expect("--tls-key");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("certificate and key");
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Some(TlsAcceptor::from(Arc::new(config)))
}

async fn serve_connection<S>(stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let builder = auto::Builder::new(TokioExecutor::new());
    let _ = builder
        .serve_connection_with_upgrades(TokioIo::new(stream), service_fn(handle))
        .await;
}

async fn serve_http(listener: TcpListener, tls: Option<TlsAcceptor>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let _ = stream.set_nodelay(true);
        let tls = tls.clone();
        tokio::spawn(async move {
            match tls {
                Some(acceptor) => match acceptor.accept(stream).await {
                    Ok(stream) => serve_connection(stream).await,
                    Err(_) => {}
                },
                None => serve_connection(stream).await,
            }
        });
    }
}

async fn serve_echo(listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let _ = stream.set_nodelay(true);
        tokio::spawn(async move {
            let (mut reader, mut writer) = TcpStream::into_split(stream);
            let _: io::Result<u64> = tokio::io::copy(&mut reader, &mut writer).await;
        });
    }
}

pub fn run(flags: &Flags) {
    let listen: Vec<SocketAddr> = flag(flags, "listen", "127.0.0.1:0".to_owned())
        .split(',')
        .map(|address| address.parse().expect("--listen address"))
        .collect();
    let echo: Option<SocketAddr> = flags
        .get("echo-listen")
        .map(|value| value.parse().expect("--echo-listen address"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(flag(flags, "threads", 2))
        .enable_all()
        .build()
        .expect("runtime");
    let tls = tls_acceptor(flags);
    runtime.block_on(async move {
        for address in listen {
            let http = bind_deep(address);
            tokio::spawn(serve_http(http, tls.clone()));
        }
        if let Some(echo) = echo {
            let listener = bind_deep(echo);
            tokio::spawn(serve_echo(listener));
        }
        println!("READY");
        std::future::pending::<()>().await;
    });
}
