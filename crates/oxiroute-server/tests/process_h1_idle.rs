#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The wait between two requests of a keep-alive HTTP/1 connection as seen on the wire: how long it may last, what ends
//! it, and that the request that follows is served as if the connection had never been idle. The proxy is the real
//! binary.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::time::{Duration, Instant};

use oxiroute_config::DownstreamTimeoutPolicy;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::{sleep, timeout},
};

use h1_wire::{Origin, STEP, Wire, find};

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: idle.test\r\n\r\n";

async fn wire_with(keepalive_ms: Option<u64>, request_ms: Option<u64>) -> Wire {
    Wire::over(Origin::start(OK).await, |config| {
        config.listeners[0].downstream_timeouts = DownstreamTimeoutPolicy {
            client_timeout_ms: request_ms,
            request_timeout_ms: request_ms,
            keepalive_timeout_ms: keepalive_ms,
        };
    })
    .await
}

/// Sends `request` and reads one complete `ok` response.
async fn serve(stream: &mut TcpStream, request: &[u8]) {
    stream.write_all(request).await.expect("write request");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 512];
    timeout(STEP, async {
        while !(find(&bytes, b"\r\n\r\n").is_some() && bytes.ends_with(b"ok")) {
            let count = stream.read(&mut buffer).await.expect("read response");
            assert!(
                count > 0,
                "the proxy closed the connection before answering"
            );
            bytes.extend_from_slice(&buffer[..count]);
        }
    })
    .await
    .expect("response");
}

/// Waits until the proxy closes the connection; returns how long it took.
async fn closed_after(stream: &mut TcpStream) -> Duration {
    let started = Instant::now();
    let mut buffer = [0_u8; 16];
    let read = timeout(STEP, stream.read(&mut buffer))
        .await
        .expect("the proxy never closed the idle connection");
    assert!(matches!(read, Ok(0) | Err(_)), "unexpected bytes: {read:?}");
    started.elapsed()
}

async fn stays_open_for(stream: &mut TcpStream, wait: Duration) -> bool {
    let mut buffer = [0_u8; 16];
    timeout(wait, stream.read(&mut buffer)).await.is_err()
}

#[tokio::test]
async fn an_idle_connection_is_closed_when_the_keep_alive_timeout_ends() {
    let wire = wire_with(Some(300), None).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    serve(&mut stream, REQUEST).await;
    let idle = closed_after(&mut stream).await;
    assert!(idle >= Duration::from_millis(200), "closed after {idle:?}");
    wire.finish();
}

#[tokio::test]
async fn a_connection_idle_for_less_than_the_timeout_serves_the_next_request() {
    let wire = wire_with(Some(1_500), None).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    serve(&mut stream, REQUEST).await;
    assert!(stays_open_for(&mut stream, Duration::from_millis(400)).await);
    serve(&mut stream, REQUEST).await;
    assert!(stays_open_for(&mut stream, Duration::from_millis(400)).await);
    serve(&mut stream, REQUEST).await;
    assert_eq!(wire.origin.requests().len(), 3);
    wire.finish();
}

#[tokio::test]
async fn the_idle_wait_does_not_count_against_the_request_timeout() {
    let wire = wire_with(Some(5_000), Some(400)).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    serve(&mut stream, REQUEST).await;
    assert!(stays_open_for(&mut stream, Duration::from_millis(900)).await);
    serve(&mut stream, REQUEST).await;
    wire.finish();
}

#[tokio::test]
async fn the_request_timeout_runs_from_the_first_byte_after_an_idle_wait() {
    let wire = wire_with(Some(5_000), Some(500)).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    serve(&mut stream, REQUEST).await;
    sleep(Duration::from_millis(200)).await;
    stream.write_all(b"GET / HT").await.expect("partial head");
    assert!(stays_open_for(&mut stream, Duration::from_millis(200)).await);
    let stalled = closed_after(&mut stream).await;
    assert!(stalled < Duration::from_secs(3), "closed after {stalled:?}");
    wire.finish();
}

#[tokio::test]
async fn a_head_split_across_the_idle_wake_up_is_parsed() {
    let wire = wire_with(None, None).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    serve(&mut stream, REQUEST).await;
    sleep(Duration::from_millis(100)).await;
    stream
        .write_all(b"GET / HTTP/1.1\r\n")
        .await
        .expect("first part");
    sleep(Duration::from_millis(100)).await;
    serve(&mut stream, b"Host: idle.test\r\n\r\n").await;
    wire.finish();
}

#[tokio::test]
async fn back_to_back_requests_on_one_connection_are_each_answered() {
    let wire = wire_with(None, None).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    for _ in 0..50 {
        serve(&mut stream, REQUEST).await;
    }
    assert_eq!(wire.origin.requests().len(), 50);
    wire.finish();
}

#[tokio::test]
async fn many_idle_connections_each_serve_their_next_request() {
    let wire = wire_with(None, None).await;
    let mut streams = Vec::new();
    for _ in 0..300 {
        let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
        serve(&mut stream, REQUEST).await;
        streams.push(stream);
    }
    for stream in streams.iter_mut().rev().step_by(3) {
        serve(stream, REQUEST).await;
    }
    for stream in streams.iter_mut().step_by(7) {
        serve(stream, REQUEST).await;
    }
    wire.finish();
}

#[tokio::test]
async fn a_client_that_closes_an_idle_connection_leaves_the_others_untouched() {
    let wire = wire_with(None, None).await;
    let mut kept = TcpStream::connect(wire.proxy).await.expect("connect");
    serve(&mut kept, REQUEST).await;
    for _ in 0..20 {
        let mut leaving = TcpStream::connect(wire.proxy).await.expect("connect");
        serve(&mut leaving, REQUEST).await;
        drop(leaving);
    }
    sleep(Duration::from_millis(200)).await;
    serve(&mut kept, REQUEST).await;
    wire.finish();
}

#[tokio::test]
async fn stopping_the_server_closes_every_idle_connection() {
    let wire = wire_with(None, None).await;
    let mut streams = Vec::new();
    for _ in 0..40 {
        let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
        serve(&mut stream, REQUEST).await;
        streams.push(stream);
    }
    let Wire { server, .. } = wire;
    server.shutdown_gracefully();
    for stream in &mut streams {
        let waited = closed_after(stream).await;
        assert!(waited < Duration::from_secs(3), "closed after {waited:?}");
    }
}
