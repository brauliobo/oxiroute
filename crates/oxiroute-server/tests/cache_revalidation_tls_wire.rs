#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Cache revalidation behind a TLS listener: an always-stale cached response is revalidated by the origin on every
//! request, and an HTTP/1.1 or HTTP/2 client keeps its connection for any number of them.

#[path = "support/revalidating_origin.rs"]
mod revalidating_origin;
mod support;

use http::{Method, StatusCode};
use oxiroute_config::{AlpnProtocol, HttpVersionPolicy};
use revalidating_origin::{BODY_LENGTH, RevalidatingOrigin, body, with_cache};
use support::{
    H2Client, PROXY_SERVER_NAME, ProxyHarness, ReservedListener, TEST_TIMEOUT, proxy_config,
    tls_connect,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    time::timeout,
};

const ROUNDS: usize = 6;

async fn proxy(alpn: Vec<AlpnProtocol>) -> (RevalidatingOrigin, ReservedListener, ProxyHarness) {
    let origin = RevalidatingOrigin::start().await;
    let reserved = ReservedListener::new();
    let mut config = proxy_config(
        reserved.address,
        origin.address,
        alpn,
        None,
        HttpVersionPolicy::default(),
    );
    with_cache(&mut config);
    let proxy = ProxyHarness::start(&config.validate(), reserved);
    (origin, reserved, proxy)
}

#[tokio::test]
async fn h2_clients_keep_their_connection_across_revalidations() {
    timeout(TEST_TIMEOUT, async {
        let (origin, reserved, proxy) = proxy(vec![AlpnProtocol::H2]).await;
        let stream = tls_connect(reserved.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 client connection");
        let mut client = H2Client::from_tls(stream).await.expect("H2 handshake");
        for round in 0..ROUNDS {
            let response = client
                .request(Method::GET, "/item")
                .await
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            assert_eq!(response.status, StatusCode::OK, "round {round}");
            assert_eq!(response.body, body(), "round {round}");
        }
        origin.assert_revalidated(ROUNDS);
        client.finish().await;
        proxy.finish().await;
        origin.finish();
    })
    .await
    .expect("HTTP/2 revalidation test timed out");
}

#[tokio::test]
async fn tls_http1_clients_keep_their_connection_across_revalidations() {
    timeout(TEST_TIMEOUT, async {
        let (origin, reserved, proxy) = proxy(vec![AlpnProtocol::Http11]).await;
        let mut stream = tls_connect(
            reserved.address,
            PROXY_SERVER_NAME,
            "ca-a.pem",
            &[b"http/1.1"],
        )
        .await
        .expect("TLS client connection");
        let request = format!("GET /item HTTP/1.1\r\nHost: {PROXY_SERVER_NAME}\r\n\r\n");
        for round in 0..ROUNDS {
            stream
                .write_all(request.as_bytes())
                .await
                .expect("write request");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let end = loop {
                let count = stream.read(&mut buffer).await.unwrap_or(0);
                assert_ne!(
                    count,
                    0,
                    "round {round}: closed after {} bytes",
                    bytes.len()
                );
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(head) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                    && bytes.len() >= head + 4 + BODY_LENGTH
                {
                    break head + 4;
                }
            };
            assert!(bytes.starts_with(b"HTTP/1.1 200"), "round {round}");
            assert_eq!(bytes[end..], body(), "round {round}");
        }
        origin.assert_revalidated(ROUNDS);
        proxy.finish().await;
        origin.finish();
    })
    .await
    .expect("TLS HTTP/1.1 revalidation test timed out");
}
