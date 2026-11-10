#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Cache revalidation as seen on the wire: the route has a memory cache and the origin answers every request with
//! a stale response (`Cache-Control: max-age=0`, `ETag`) and 304 to a matching `If-None-Match`, so every request
//! after the first revalidates. Any number of revalidations must be served from the cache entry on one keep-alive
//! client connection. The proxy is the real binary, with the HTTP/1.1 fast lane on and off.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/revalidating_origin.rs"]
mod revalidating_origin;

use std::sync::Arc;

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

use h1_wire::{Origin, STEP, Wire, find};
use revalidating_origin::{RevalidatingOrigin, body, with_cache};

const ROUNDS: usize = 6;
const PLAIN: &str = "GET /item HTTP/1.1\r\nHost: cache.test\r\n\r\n";
const MAX_AGE_ZERO: &str =
    "GET /item HTTP/1.1\r\nHost: cache.test\r\nCache-Control: max-age=0\r\n\r\n";

/// One response framed by `Content-Length`.
async fn read_response(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let head_end = loop {
        if let Some(position) = find(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response head")
            .expect("read");
        assert_ne!(count, 0, "closed before the head: {bytes:?}");
        bytes.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let length = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < head_end + length {
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response body")
            .expect("read");
        assert_ne!(count, 0, "closed inside the body of {head}");
        bytes.extend_from_slice(&buffer[..count]);
    }
    (head, bytes[head_end..].to_vec())
}

/// `request` repeated on one connection; every response is the cached 200 with its body, every origin request a revalidation.
async fn revalidations_on_one_connection(fast_lane: bool, request: &str) {
    let origin = RevalidatingOrigin::start().await;
    let origin_address = origin.address;
    let wire = Wire::over(
        Origin {
            address: origin_address,
            requests: Arc::default(),
        },
        |config| {
            config.http_fast_lane = Some(fast_lane);
            with_cache(config);
        },
    )
    .await;
    let proxy = wire.proxy;
    let rounds = tokio::spawn({
        let request = request.to_owned();
        async move {
            let mut client = TcpStream::connect(proxy).await.expect("connect");
            for round in 0..ROUNDS {
                client.write_all(request.as_bytes()).await.expect("write");
                let (head, response_body) = read_response(&mut client).await;
                assert!(head.starts_with("HTTP/1.1 200 OK"), "round {round}: {head}");
                assert_eq!(response_body, body(), "round {round}");
            }
        }
    });
    rounds
        .await
        .expect("every revalidation is answered on the one connection");
    origin.assert_revalidated(ROUNDS);
    wire.finish();
    origin.finish();
}

#[tokio::test]
async fn revalidating_requests_keep_the_connection_on_the_fast_lane_path() {
    revalidations_on_one_connection(true, PLAIN).await;
}

#[tokio::test]
async fn revalidating_requests_keep_the_connection_on_the_classic_path() {
    revalidations_on_one_connection(false, PLAIN).await;
}

#[tokio::test]
async fn max_age_zero_requests_keep_the_connection_with_the_fast_lane() {
    revalidations_on_one_connection(true, MAX_AGE_ZERO).await;
}

#[tokio::test]
async fn max_age_zero_requests_keep_the_connection_on_the_classic_path() {
    revalidations_on_one_connection(false, MAX_AGE_ZERO).await;
}
