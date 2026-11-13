#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! What an idle connection costs the proxy, as anonymous memory of the real process per connection: a plain WebSocket
//! after its handshake and an HTTP/1 keep-alive connection after one request. The ceilings are the measured cost plus
//! a margin; a change that makes a connection hold more (a buffer kept while it waits, state of the request kept for
//! the life of an upgrade) fails here instead of in a benchmark. Allocation sizes are the same on every host, so
//! the test does not depend on timing or load, and an unoptimized build measures the same.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, time::Duration};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::{sleep, timeout},
};

use h1_wire::{Origin, STEP, Wire, find};

const WARM: usize = 50;
const MEASURED: usize = 300;
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: idle.test\r\n\r\n";
const SWITCHING: &[u8] =
    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
    Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
const UPGRADE: &[u8] =
    b"GET /socket HTTP/1.1\r\nHost: idle.test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
    Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";

/// Bytes per connection, measured with 300 connections after 50 warm ones: 13.0 kB for a WebSocket and 3.2 kB for an
/// HTTP/1 connection. A read buffer kept while the connection waits adds 8 kB to either.
const WEBSOCKET_CEILING: usize = 14_000;
const HTTP1_CEILING: usize = 4_500;

fn anonymous_bytes(pid: u32) -> usize {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).expect("process status");
    let kilobytes = status
        .lines()
        .find_map(|line| line.strip_prefix("RssAnon:"))
        .and_then(|rest| {
            rest.trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<usize>()
                .ok()
        })
        .expect("RssAnon of the process");
    kilobytes * 1024
}

/// Reads until `end` is the tail of what arrived.
async fn read_until(stream: &mut TcpStream, end: &[u8]) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 512];
    timeout(STEP, async {
        while !bytes.ends_with(end) {
            let count = stream.read(&mut buffer).await.expect("read");
            assert!(count > 0, "the proxy closed the connection");
            bytes.extend_from_slice(&buffer[..count]);
        }
    })
    .await
    .expect("response");
}

/// Opens `count` connections, each of which sends `request` and reads the answer up to `end`, and keeps them open.
async fn open(wire: &Wire, count: usize, request: &[u8], end: &[u8]) -> Vec<TcpStream> {
    let mut streams = Vec::with_capacity(count);
    for _ in 0..count {
        let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
        stream.write_all(request).await.expect("write request");
        read_until(&mut stream, end).await;
        streams.push(stream);
    }
    streams
}

/// The anonymous memory each of `MEASURED` idle connections adds, after `WARM` others made the process set up what it
/// sets up once.
async fn idle_bytes_per_connection(reply: &'static [u8], request: &[u8], end: &[u8]) -> usize {
    let wire = Wire::start(reply).await;
    let pid = wire.server.pid();
    let warm = open(&wire, WARM, request, end).await;
    sleep(Duration::from_secs(1)).await;
    let before = anonymous_bytes(pid);
    let measured = open(&wire, MEASURED, request, end).await;
    sleep(Duration::from_secs(1)).await;
    let after = anonymous_bytes(pid);
    drop((warm, measured));
    after.saturating_sub(before) / MEASURED
}

#[tokio::test]
async fn an_idle_websocket_holds_no_more_than_its_ceiling() {
    let bytes = idle_bytes_per_connection(SWITCHING, UPGRADE, b"\r\n\r\n").await;
    println!("idle websocket: {bytes} bytes per connection");
    assert!(
        bytes <= WEBSOCKET_CEILING,
        "an idle WebSocket holds {bytes} bytes, ceiling {WEBSOCKET_CEILING}"
    );
}

#[tokio::test]
async fn an_idle_keepalive_connection_holds_no_more_than_its_ceiling() {
    let bytes = idle_bytes_per_connection(OK, REQUEST, b"ok").await;
    println!("idle keep-alive: {bytes} bytes per connection");
    assert!(
        bytes <= HTTP1_CEILING,
        "an idle HTTP/1 connection holds {bytes} bytes, ceiling {HTTP1_CEILING}"
    );
}
