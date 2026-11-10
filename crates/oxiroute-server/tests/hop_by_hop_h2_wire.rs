#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Hop-by-hop fields of an HTTP/2 client's requests and of the origin's responses (RFC 9110 section 7.6.1,
//! RFC 9113 section 8.2.2): HTTP/2 itself refuses `Connection`, `Keep-Alive`, `Proxy-Connection`,
//! `Transfer-Encoding` and `Upgrade`, so what reaches the proxy is `TE: trailers`, the proxy credentials fields
//! and `Trailer`. The proxy is in front of a plain HTTP/1.1 origin or an HTTP/2 one.

mod support;

use std::{net::SocketAddr, sync::Arc};

use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use oxiroute_config::{AlpnProtocol, HttpVersion, HttpVersionPolicy};
use support::{
    H2Client, ORIGIN_SERVER_NAME, PROXY_SERVER_NAME, ProxyHarness, ReservedListener, TEST_TIMEOUT,
    TlsOrigin, proxy_config, tls_connect, verified_upstream,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

const HOP_BY_HOP: [(&str, &str); 4] = [
    ("te", "trailers"),
    ("proxy-authorization", "Basic eA=="),
    ("proxy-authenticate", "Basic"),
    ("trailer", "x-sum"),
];

fn request_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in HOP_BY_HOP {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    headers.insert("x-keep", HeaderValue::from_static("1"));
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer kept"),
    );
    headers
}

/// A plain HTTP/1.1 origin that answers every request with `reply` and records each request head.
struct RawOrigin {
    address: SocketAddr,
    heads: Arc<std::sync::Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl RawOrigin {
    async fn start(reply: &'static [u8]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let heads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&heads);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    loop {
                        while let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&bytes[..end + 4]).into_owned();
                            seen.lock().expect("heads").push(head);
                            bytes.drain(..end + 4);
                            if stream.write_all(reply).await.is_err() {
                                return;
                            }
                        }
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                        }
                    }
                });
            }
        });
        Self {
            address,
            heads,
            task,
        }
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().expect("heads").clone()
    }
}

fn lines(head: &str, name: &str) -> Vec<String> {
    head.split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(": "))
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.to_owned())
        .collect()
}

async fn h2_client(reserved: &ReservedListener) -> H2Client {
    let stream = tls_connect(reserved.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
        .await
        .expect("H2 client connection");
    H2Client::from_tls(stream)
        .await
        .expect("H2 client handshake")
}

/// An HTTP/2 client in front of the HTTP/1.1 origin `origin`; `run` gets the client and the origin.
async fn over_h1_origin<F, Fut>(reply: &'static [u8], run: F)
where
    F: FnOnce(H2Client, Arc<RawOrigin>) -> Fut,
    Fut: std::future::Future<Output = H2Client>,
{
    timeout(TEST_TIMEOUT, async {
        let origin = Arc::new(RawOrigin::start(reply).await);
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let client = h2_client(&reserved).await;
        let client = run(client, Arc::clone(&origin)).await;
        client.finish().await;
        proxy.finish().await;
        origin.task.abort();
    })
    .await
    .expect("HTTP/2 hop-by-hop test timed out");
}

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

#[tokio::test]
async fn h2_request_hop_by_hop_headers_do_not_reach_an_h1_origin() {
    over_h1_origin(OK, |mut client, origin| async move {
        let response = client
            .request_with_headers(Method::GET, "/r", request_headers())
            .await
            .expect("response");
        assert_eq!(response.status, StatusCode::OK);
        let head = origin.heads().remove(0);
        for (name, _) in HOP_BY_HOP {
            assert!(
                lines(&head, name).is_empty(),
                "{name} reached the origin: {head}"
            );
        }
        assert_eq!(lines(&head, "x-keep"), ["1"], "{head}");
        assert_eq!(lines(&head, "authorization"), ["Bearer kept"], "{head}");
        client
    })
    .await;
}

#[tokio::test]
async fn h2_response_hop_by_hop_headers_do_not_reach_the_client() {
    over_h1_origin(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive, X-Private\r\nX-Private: 1\r\nKeep-Alive: timeout=5\r\nProxy-Authenticate: Basic\r\nTE: trailers\r\nX-Kept: 1\r\n\r\nok",
        |mut client, _| async move {
            let response = client
                .request_with_headers(Method::GET, "/r", HeaderMap::new())
                .await
                .expect("response");
            assert_eq!(response.status, StatusCode::OK);
            for name in ["x-private", "keep-alive", "proxy-authenticate", "te", "connection"] {
                assert!(!response.headers.contains_key(name), "{name}: {:?}", response.headers);
            }
            assert_eq!(response.headers["x-kept"], "1");
            assert_eq!(response.body, b"ok".as_slice());
            client
        },
    )
    .await;
}

#[tokio::test]
async fn h2_authority_and_host_that_differ_are_refused() {
    over_h1_origin(OK, |mut client, origin| async move {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("other.example.test"));
        let response = client
            .request_with_headers(Method::GET, "/r", headers)
            .await
            .expect("response");
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(origin.heads().is_empty());
        client
    })
    .await;
}

#[tokio::test]
async fn h2_te_trailers_reaches_an_h2_origin_and_the_other_hop_by_hop_headers_do_not() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let mut client = h2_client(&reserved).await;
        let mut headers = request_headers();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/grpc"),
        );
        let response = client
            .request_with_headers(Method::GET, "/grpc/full", headers)
            .await
            .expect("response");
        assert_eq!(response.status, StatusCode::OK);
        let seen = origin.observations.h2_request_headers().remove(0);
        assert_eq!(seen["te"], "trailers", "{seen:?}");
        for name in [
            "proxy-authorization",
            "proxy-authenticate",
            "trailer",
            "connection",
        ] {
            assert!(!seen.contains_key(name), "{name}: {seen:?}");
        }
        assert_eq!(seen["x-keep"], "1");
        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("HTTP/2 upstream hop-by-hop test timed out");
}
