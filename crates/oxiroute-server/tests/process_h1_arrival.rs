#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The first request of a new HTTP/1 connection as seen on the wire: it may be in the socket when the proxy first looks
//! (sent with the connect or split) or arrive later, and a connection that never sends one ends by the
//! request timeout or when the client leaves. The proxy is the real binary.

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
const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: arrival.test\r\n\r\n";
const CLOSING: &[u8] = b"GET / HTTP/1.1\r\nHost: arrival.test\r\nConnection: close\r\n\r\n";

/// Reads until `responses` complete `ok` responses came back or the proxy closed the connection.
async fn read_responses(stream: &mut TcpStream, responses: usize) -> usize {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    timeout(STEP, async {
        while bytes.windows(2).filter(|pair| pair == b"ok").count() < responses {
            match stream.read(&mut buffer).await {
                Ok(count) if count > 0 => bytes.extend_from_slice(&buffer[..count]),
                _ => break,
            }
        }
    })
    .await
    .expect("responses");
    bytes
        .windows(15)
        .filter(|part| *part == b"HTTP/1.1 200 OK")
        .count()
}

#[tokio::test]
async fn a_request_sent_with_the_connection_is_served() {
    let wire = Wire::start(OK).await;
    for _ in 0..20 {
        let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
        stream.write_all(CLOSING).await.expect("request");
        assert_eq!(read_responses(&mut stream, 1).await, 1);
    }
    assert_eq!(wire.origin.requests().len(), 20);
    wire.finish();
}

#[tokio::test]
async fn a_request_sent_after_a_pause_is_served() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    sleep(Duration::from_millis(250)).await;
    stream.write_all(REQUEST).await.expect("request");
    assert_eq!(read_responses(&mut stream, 1).await, 1);
    wire.finish();
}

#[tokio::test]
async fn a_head_split_across_the_first_reads_is_served() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHo")
        .await
        .expect("first part");
    sleep(Duration::from_millis(100)).await;
    stream
        .write_all(b"st: arrival.test\r\n")
        .await
        .expect("second part");
    sleep(Duration::from_millis(100)).await;
    stream.write_all(b"\r\n").await.expect("end of head");
    assert_eq!(read_responses(&mut stream, 1).await, 1);
    wire.finish();
}

#[tokio::test]
async fn a_connection_that_sends_nothing_ends_by_the_request_timeout() {
    let wire = Wire::over(Origin::start(OK).await, |config| {
        config.listeners[0].downstream_timeouts = DownstreamTimeoutPolicy {
            client_timeout_ms: Some(400),
            request_timeout_ms: Some(400),
            keepalive_timeout_ms: None,
        };
    })
    .await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    let started = Instant::now();
    let mut buffer = [0_u8; 16];
    let read = timeout(STEP, stream.read(&mut buffer))
        .await
        .expect("the proxy never closed the silent connection");
    assert!(matches!(read, Ok(0) | Err(_)), "unexpected bytes: {read:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "closed after {:?}",
        started.elapsed()
    );
    // the proxy serves the connections that follow
    let mut next = TcpStream::connect(wire.proxy).await.expect("connect");
    next.write_all(CLOSING).await.expect("request");
    assert_eq!(read_responses(&mut next, 1).await, 1);
    wire.finish();
}

#[tokio::test]
async fn connections_that_leave_without_a_request_do_not_disturb_the_next_ones() {
    let wire = Wire::start(OK).await;
    for _ in 0..10 {
        drop(TcpStream::connect(wire.proxy).await.expect("connect"));
    }
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    stream.write_all(CLOSING).await.expect("request");
    assert_eq!(read_responses(&mut stream, 1).await, 1);
    assert_eq!(wire.origin.requests().len(), 1);
    wire.finish();
}

#[tokio::test]
async fn a_request_followed_by_an_immediate_close_is_served_without_hurting_the_proxy() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    stream.write_all(REQUEST).await.expect("request");
    drop(stream);
    sleep(Duration::from_millis(200)).await;
    let mut next = TcpStream::connect(wire.proxy).await.expect("connect");
    next.write_all(CLOSING).await.expect("request");
    assert_eq!(read_responses(&mut next, 1).await, 1);
    wire.finish();
}

#[tokio::test]
async fn a_closing_response_is_followed_by_the_end_of_the_stream() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    stream.write_all(CLOSING).await.expect("request");
    let mut response = Vec::new();
    timeout(STEP, stream.read_to_end(&mut response))
        .await
        .expect("the proxy never ended the connection")
        .expect("read");
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(
        response.ends_with(b"\r\n\r\nok"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    wire.finish();
}
