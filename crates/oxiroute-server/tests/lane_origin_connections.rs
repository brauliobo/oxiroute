#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! How many connections a proxied HTTP/1 request costs the origin, on the fast lane and on the classic path alike:
//! an idle origin connection is reused, one the origin closed while idle is not, and concurrent requests open only
//! the connections they need. The proxy is the real binary, the origin a raw TCP server that counts what it accepts.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};

use h1_wire::{Origin, STEP, Wire, find, read_request};

const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: pool.test\r\n\r\n";

/// An origin that counts the connections it accepts and, when `idle_close` is set, closes each one that stays
/// quiet that long after a reply.
struct CountingOrigin {
    origin: Origin,
    accepted: Arc<AtomicUsize>,
}

impl CountingOrigin {
    async fn start(idle_close: Option<Duration>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    loop {
                        let request = match idle_close {
                            Some(limit) => timeout(limit, read_request(&mut stream))
                                .await
                                .ok()
                                .flatten(),
                            None => read_request(&mut stream).await,
                        };
                        if request.is_none() || stream.write_all(REPLY).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            origin: Origin {
                address,
                requests: Arc::default(),
            },
            accepted,
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

async fn proxy_for(origin: CountingOrigin, lane: bool) -> (Wire, Arc<AtomicUsize>) {
    let accepted = Arc::clone(&origin.accepted);
    let wire = Wire::over(origin.origin, |config| {
        config.http_fast_lane = Some(lane);
    })
    .await;
    (wire, accepted)
}

/// Sends `request` on `stream` and reads one complete `ok` response.
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
    assert!(bytes.starts_with(b"HTTP/1.1 200 OK"), "{bytes:?}");
}

async fn client(wire: &Wire) -> TcpStream {
    TcpStream::connect(wire.proxy).await.expect("connect")
}

/// Requests one after the other, on one client connection and on new ones, share the origin connection.
async fn sequential_requests_share_an_origin_connection(lane: bool) {
    let (wire, accepted) = proxy_for(CountingOrigin::start(None).await, lane).await;
    let mut kept = client(&wire).await;
    for _ in 0..20 {
        serve(&mut kept, REQUEST).await;
    }
    for _ in 0..5 {
        let mut fresh = client(&wire).await;
        serve(&mut fresh, REQUEST).await;
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "lane: {lane}");
    wire.finish();
}

#[tokio::test]
async fn lane_requests_share_an_origin_connection() {
    sequential_requests_share_an_origin_connection(true).await;
}

#[tokio::test]
async fn classic_requests_share_an_origin_connection() {
    sequential_requests_share_an_origin_connection(false).await;
}

/// An origin connection the origin closed while it was idle is never used for a request.
async fn a_connection_closed_while_idle_is_not_reused(lane: bool) {
    let (wire, accepted) = proxy_for(
        CountingOrigin::start(Some(Duration::from_millis(150))).await,
        lane,
    )
    .await;
    let mut stream = client(&wire).await;
    serve(&mut stream, REQUEST).await;
    sleep(Duration::from_millis(600)).await;
    serve(&mut stream, REQUEST).await;
    serve(&mut stream, REQUEST).await;
    assert_eq!(accepted.load(Ordering::SeqCst), 2, "lane: {lane}");
    wire.finish();
}

#[tokio::test]
async fn lane_does_not_reuse_a_connection_the_origin_closed() {
    a_connection_closed_while_idle_is_not_reused(true).await;
}

#[tokio::test]
async fn classic_does_not_reuse_a_connection_the_origin_closed() {
    a_connection_closed_while_idle_is_not_reused(false).await;
}

/// Concurrent clients open at most one origin connection each, and the next round reuses all of them.
async fn concurrent_requests_open_what_they_need(lane: bool) {
    const CLIENTS: usize = 8;
    let (wire, accepted) = proxy_for(CountingOrigin::start(None).await, lane).await;
    let wire = Arc::new(wire);
    let mut rounds = Vec::new();
    for round in 0..2 {
        let mut tasks = Vec::new();
        for _ in 0..CLIENTS {
            let wire = Arc::clone(&wire);
            tasks.push(tokio::spawn(async move {
                let mut stream = client(&wire).await;
                for _ in 0..10 {
                    serve(&mut stream, REQUEST).await;
                }
            }));
        }
        for task in tasks {
            task.await.expect("client");
        }
        rounds.push(accepted.load(Ordering::SeqCst));
        assert!(
            rounds[round] <= CLIENTS,
            "lane: {lane}, round {round}: {rounds:?}"
        );
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        rounds[0], rounds[1],
        "the second round reuses the first one's connections (lane: {lane})"
    );
    Arc::into_inner(wire).expect("wire").finish();
}

#[tokio::test]
async fn lane_concurrent_requests_open_what_they_need() {
    concurrent_requests_open_what_they_need(true).await;
}

#[tokio::test]
async fn classic_concurrent_requests_open_what_they_need() {
    concurrent_requests_open_what_they_need(false).await;
}
