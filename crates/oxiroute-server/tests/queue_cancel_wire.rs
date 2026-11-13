#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! A client that leaves while its request waits for a slot of a saturated pool takes its place in the queue with it
//! and the origin never receives the request. A real `oxiroute` process in front of an origin that holds its requests
//! until the test releases them, with a pool of one slot and a queue: the first request holds the slot, the second
//! one queues behind it and its client goes away. When the slot frees up, only requests that are still wanted reach
//! the origin, and the pool's queue is empty again. The slot is free when the next request is answered (the idle pooled
//! origin connection still counts as active).

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod wire_support;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use http::{Method, Request};
use oxiroute_config::{
    AlpnProtocol, ConfigDraft, DnsResolutionPolicy, HttpVersionPolicy, ListenerBind, Management,
    Protocol, TlsVersion, UpstreamServer,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::{sleep, timeout},
};

use config_support::socket_endpoint;
use h1_wire::{find, read_request};
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address};
use wire_support::{H2Client, PROXY_SERVER_NAME, tls_connect};

const STEP: Duration = Duration::from_secs(10);
const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";
const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const SERVED_BY_LANE: &str = "served by the HTTP/";
const LEFT: &str = "client left before its request reached an origin";

/// An origin that holds every request until the test releases it, and records the path of each one it received.
struct Origin {
    address: SocketAddr,
    paths: Arc<Mutex<Vec<String>>>,
    gate: Arc<Semaphore>,
}

impl Origin {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let paths = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Semaphore::new(0));
        let (seen, held) = (Arc::clone(&paths), Arc::clone(&gate));
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                let (seen, held) = (Arc::clone(&seen), Arc::clone(&held));
                tokio::spawn(async move {
                    while let Some(request) = read_request(&mut stream).await {
                        let line = String::from_utf8_lossy(&request).into_owned();
                        let path = line.split(' ').nth(1).unwrap_or_default().to_owned();
                        seen.lock().expect("paths").push(path);
                        held.acquire().await.expect("gate open").forget();
                        if stream.write_all(REPLY).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            address,
            paths,
            gate,
        }
    }

    fn release(&self, count: usize) {
        self.gate.add_permits(count);
    }

    fn seen(&self) -> Vec<String> {
        self.paths.lock().expect("paths").clone()
    }

    async fn arrived(&self, path: &str) {
        timeout(STEP, async {
            while !self.seen().iter().any(|seen| seen == path) {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the origin never received {path}"));
    }
}

/// What the pool reports about its queue.
#[derive(Debug)]
struct Snapshot {
    queued: u64,
    cancelled: u64,
}

struct Farm {
    origin: Origin,
    proxy: SocketAddr,
    management: SocketAddr,
    server: ServerProcess,
}

/// A pool of one server that serves one request at a time and queues the others for as long as a test needs.
fn saturated(config: &mut ConfigDraft, origin: SocketAddr, management: SocketAddr, lane: bool) {
    config.management = Some(Management {
        bind: management,
        ui_dir: None,
        token_file: None,
    });
    config.http_fast_lane = (!lane).then_some(false);
    config.http_services[0].upstream_io_timeout_ms = 30_000;
    config.log_filter =
        Some("oxiroute::lane=debug,oxiroute::h2lane=debug,oxiroute::queue=debug".into());
    let pool = &mut config.upstream_pools[0];
    pool.queue_timeout_ms = Some(60_000);
    pool.endpoints.clear();
    pool.servers = vec![UpstreamServer {
        name: "origin".into(),
        endpoint: socket_endpoint(origin),
        max_connections: Some(1),
        dns_resolution: DnsResolutionPolicy::OnConnect,
    }];
}

impl Farm {
    /// Plain HTTP/1.1 listener.
    async fn h1(lane: bool) -> Self {
        let origin = Origin::start().await;
        let (proxy, management) = (reserve_tcp_address(), reserve_tcp_address());
        let mut config = h1_wire::config(proxy, origin.address);
        saturated(&mut config, origin.address, management, lane);
        let validated = config.validate().expect("valid config");
        Self::run(origin, (proxy, management), &validated).await
    }

    /// TLS listener that speaks HTTP/2.
    async fn h2(lane: bool) -> Self {
        let origin = Origin::start().await;
        let (proxy, management) = (reserve_tcp_address(), reserve_tcp_address());
        let mut config = wire_support::proxy_config(
            proxy,
            origin.address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        saturated(&mut config, origin.address, management, lane);
        let validated = config.validate();
        Self::run(origin, (proxy, management), &validated).await
    }

    async fn run(
        origin: Origin,
        (proxy, management): (SocketAddr, SocketAddr),
        config: &oxiroute_config::ValidatedConfig,
    ) -> Self {
        let mut server = ServerProcess::start(config, Some(TOKEN));
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(proxy).await;
        Self {
            origin,
            proxy,
            management,
            server,
        }
    }

    async fn snapshot(&self) -> Snapshot {
        let response = http_request(self.management, "GET", "/metrics", &[], &[]).await;
        let text = String::from_utf8(response.body).expect("metrics UTF-8");
        let value = |name: &str| -> u64 {
            text.lines()
                .filter(|line| {
                    line.starts_with(&format!("{name}{{")) && line.contains("pool=\"origin\"")
                })
                .find_map(|line| line.rsplit(' ').next()?.parse::<u64>().ok())
                .unwrap_or_else(|| panic!("no {name} for the pool in {text}"))
        };
        Snapshot {
            queued: value("oxiroute_pool_queue_depth"),
            cancelled: value("oxiroute_pool_queue_cancellations_total"),
        }
    }

    async fn until(&self, what: &str, done: impl Fn(&Snapshot) -> bool) {
        let outcome = timeout(STEP, async {
            loop {
                if done(&self.snapshot().await) {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            outcome.is_ok(),
            "never saw {what}: {:?}",
            self.snapshot().await
        );
    }

    /// Stops the process; the number of requests it served by a lane. `named` is how many debug lines say that a
    /// client left before its request reached an origin.
    fn finish(self, named: usize) -> usize {
        let output = self.server.shutdown_output();
        let left = output.lines().filter(|line| line.contains(LEFT)).count();
        assert_eq!(left, named, "lines that say a client left: {output}");
        output
            .lines()
            .filter(|line| line.contains(SERVED_BY_LANE))
            .count()
    }
}

async fn read_reply(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    timeout(STEP, async {
        while find(&bytes, b"\r\n\r\nok").is_none() {
            let count = stream.read(&mut buffer).await.expect("read reply");
            assert_ne!(count, 0, "closed before the reply: {bytes:?}");
            bytes.extend_from_slice(&buffer[..count]);
        }
    })
    .await
    .expect("reply in time");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A holds the slot, B queues and leaves, C comes after: the origin receives A and C and nothing else.
async fn h1_client_leaves(lane: bool, queued: &[u8]) {
    let farm = Farm::h1(lane).await;
    let mut holder = TcpStream::connect(farm.proxy).await.expect("connect");
    holder
        .write_all(b"GET /a HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .await
        .expect("send A");
    farm.origin.arrived("/a").await;

    let mut leaver = TcpStream::connect(farm.proxy).await.expect("connect");
    leaver.write_all(queued).await.expect("send B");
    farm.until("B in the queue", |pool| pool.queued == 1).await;
    drop(leaver);
    farm.until("B out of the queue", |pool| {
        pool.queued == 0 && pool.cancelled == 1
    })
    .await;

    farm.origin.release(1);
    assert!(read_reply(&mut holder).await.starts_with("HTTP/1.1 200"));

    // A request that comes after B is answered first when B is gone from the queue.
    farm.origin.release(1);
    holder
        .write_all(b"GET /c HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .await
        .expect("send C");
    assert!(read_reply(&mut holder).await.starts_with("HTTP/1.1 200"));
    assert_eq!(farm.origin.seen(), ["/a", "/c"]);
    farm.until("the pool at rest", |pool| {
        pool.queued == 0 && pool.cancelled == 1
    })
    .await;
    let served = farm.finish(1);
    assert_eq!(served > 0, lane, "lane served {served} requests");
}

const GET_B: &[u8] = b"GET /b HTTP/1.1\r\nHost: example.test\r\n\r\n";
const POST_B_WITHOUT_BODY: &[u8] =
    b"POST /b HTTP/1.1\r\nHost: example.test\r\nContent-Length: 10\r\n\r\n";

#[tokio::test]
async fn h1_lane_request_is_dropped_from_the_queue_when_its_client_leaves() {
    h1_client_leaves(true, GET_B).await;
}

#[tokio::test]
async fn h1_classic_request_is_dropped_from_the_queue_when_its_client_leaves() {
    h1_client_leaves(false, GET_B).await;
}

#[tokio::test]
async fn h1_lane_request_with_an_announced_body_is_dropped_when_its_client_leaves() {
    h1_client_leaves(true, POST_B_WITHOUT_BODY).await;
}

#[tokio::test]
async fn h1_classic_request_with_an_announced_body_is_dropped_when_its_client_leaves() {
    h1_client_leaves(false, POST_B_WITHOUT_BODY).await;
}

/// What B's HTTP/2 client does once its request waits.
#[derive(Clone, Copy)]
enum Leave {
    /// Resets the stream of a request that ended with its head (the lane serves those).
    Reset,
    /// Resets the stream of a request whose body is still open, which the classic path takes over from the lane.
    ResetOpenBody,
    CloseConnection,
}

/// Same, over HTTP/2.
async fn h2_client_leaves(lane: bool, leave: Leave) {
    let farm = Farm::h2(lane).await;
    let connect = || async {
        let stream = tls_connect(farm.proxy, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 client connection");
        H2Client::from_tls(stream)
            .await
            .expect("H2 client handshake")
    };
    let mut holder = connect().await;
    let (held, _held_body) = holder
        .start_request(Method::GET, "/a")
        .await
        .expect("send A");
    farm.origin.arrived("/a").await;

    match leave {
        Leave::CloseConnection => {
            let mut leaver = connect().await;
            let (_queued, _queued_body) = leaver
                .start_request(Method::GET, "/b")
                .await
                .expect("send B");
            farm.until("B in the queue", |pool| pool.queued == 1).await;
            leaver.finish().await;
        }
        Leave::Reset => {
            let (_queued, mut queued_body) = holder
                .start_request(Method::GET, "/b")
                .await
                .expect("send B");
            farm.until("B in the queue", |pool| pool.queued == 1).await;
            queued_body.send_reset(h2::Reason::CANCEL);
        }
        Leave::ResetOpenBody => holder
            .cancel_request(
                Method::GET,
                "/b",
                farm.until("B in the queue", |pool| pool.queued == 1),
            )
            .await
            .expect("reset B"),
    }
    farm.until("B out of the queue", |pool| {
        pool.queued == 0 && pool.cancelled == 1
    })
    .await;

    farm.origin.release(1);
    let answer = timeout(STEP, held)
        .await
        .expect("A answered")
        .expect("A response");
    assert_eq!(answer.status(), 200);

    farm.origin.release(1);
    let answer = holder.request(Method::GET, "/c").await.expect("C response");
    assert_eq!(answer.status.as_u16(), 200);
    assert_eq!(farm.origin.seen(), ["/a", "/c"]);
    farm.until("the pool at rest", |pool| {
        pool.queued == 0 && pool.cancelled == 1
    })
    .await;
    let served = farm.finish(1);
    assert_eq!(served > 0, lane, "lane served {served} requests");
}

#[tokio::test]
async fn h2_lane_stream_reset_drops_the_request_from_the_queue() {
    h2_client_leaves(true, Leave::Reset).await;
}

#[tokio::test]
async fn h2_lane_stream_reset_with_an_open_body_drops_the_request_from_the_queue() {
    h2_client_leaves(true, Leave::ResetOpenBody).await;
}

#[tokio::test]
async fn h2_lane_connection_close_drops_the_request_from_the_queue() {
    h2_client_leaves(true, Leave::CloseConnection).await;
}

#[tokio::test]
async fn h2_classic_stream_reset_drops_the_request_from_the_queue() {
    h2_client_leaves(false, Leave::Reset).await;
}

#[tokio::test]
async fn h2_classic_stream_reset_with_an_open_body_drops_the_request_from_the_queue() {
    h2_client_leaves(false, Leave::ResetOpenBody).await;
}

#[tokio::test]
async fn h2_classic_connection_close_drops_the_request_from_the_queue() {
    h2_client_leaves(false, Leave::CloseConnection).await;
}

/// Same, over HTTP/3, which has no lane: the request is held by the shared upstream selection.
async fn h3_client_leaves(close_connection: bool) {
    let origin = Origin::start().await;
    let (udp, management) = (wire_support::ports::unused_address(), reserve_tcp_address());
    let mut config = wire_support::proxy_config(
        udp,
        origin.address,
        vec![AlpnProtocol::H3],
        None,
        HttpVersionPolicy::default(),
    );
    saturated(&mut config, origin.address, management, true);
    config.listeners[0].bind = ListenerBind::Udp { address: udp };
    config.listeners[0].protocol = Protocol::Http3;
    config.tls_profiles[0].min_version = TlsVersion::Tls13;
    config.http_services[0].routes[0]
        .policy
        .max_request_body_bytes = Some(64 * 1024);
    config.http_services[0].routes[0].policy.request_buffering = true;
    let validated = config.validate();
    let mut server = ServerProcess::start(&validated, Some(TOKEN));
    server.wait_for_tcp(management).await;
    let farm = Farm {
        origin,
        proxy: management,
        management,
        server,
    };

    let endpoint = wire_support::h3::client_endpoint(
        &fixture_support::fixture("ca-a.pem"),
        wire_support::h3::H3_ALPN,
    )
    .expect("H3 client endpoint");
    let connect = || async {
        let connection = wire_support::h3::connect(&endpoint, udp, PROXY_SERVER_NAME).await;
        let (driver, sender) = h3::client::new(h3_quinn::Connection::new(connection.clone()))
            .await
            .expect("H3 client connection");
        (connection, sender, wire_support::h3::drive_client(driver))
    };
    let get = |path: &str| {
        Request::builder()
            .method(Method::GET)
            .uri(format!("https://example.test{path}"))
            .body(())
            .expect("H3 request")
    };

    let (_holder_connection, mut holder, _driver) = connect().await;
    let mut held = holder.send_request(get("/a")).await.expect("send A");
    held.finish().await.expect("finish A");
    farm.origin.arrived("/a").await;

    let (leaver_connection, mut leaver, _leaver_driver) = connect().await;
    let mut queued = leaver.send_request(get("/b")).await.expect("send B");
    queued.finish().await.expect("finish B");
    farm.until("B in the queue", |pool| pool.queued == 1).await;
    if close_connection {
        leaver_connection.close(quinn::VarInt::from_u32(0), b"gone");
    } else {
        queued.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
    }
    farm.until("B out of the queue", |pool| {
        pool.queued == 0 && pool.cancelled == 1
    })
    .await;

    farm.origin.release(1);
    let answer = timeout(STEP, held.recv_response())
        .await
        .expect("A answered");
    assert_eq!(answer.expect("A response").status(), 200);

    farm.origin.release(1);
    let mut next = holder.send_request(get("/c")).await.expect("send C");
    next.finish().await.expect("finish C");
    let answer = timeout(STEP, next.recv_response())
        .await
        .expect("C answered");
    assert_eq!(answer.expect("C response").status(), 200);
    assert_eq!(farm.origin.seen(), ["/a", "/c"]);
    farm.until("the pool at rest", |pool| {
        pool.queued == 0 && pool.cancelled == 1
    })
    .await;
    // A lost HTTP/3 connection drops the whole request task, which frees its place without a word.
    farm.finish(usize::from(!close_connection));
}

#[tokio::test]
async fn h3_stream_cancel_drops_the_request_from_the_queue() {
    h3_client_leaves(false).await;
}

#[tokio::test]
async fn h3_connection_close_drops_the_request_from_the_queue() {
    h3_client_leaves(true).await;
}
