#![cfg(feature = "alloc-count")]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Allocator calls per request of the HTTP/1 fast lane. The proxy is the real binary built with the counting allocator
//! (`--features alloc-count`); a keep-alive connection sends requests, and the calls between two snapshots of the
//! counters, over the requests between them, are compared with a ceiling per route shape. A ceiling is a few calls
//! above what the shape costs today: a change that adds an allocation to the request path has to raise it on purpose.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, path::Path, process::Command, time::Duration};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, HttpProxyPolicy, HttpRequestHeaderMutation,
    HttpRequestHeaderValue, HttpResponseHeaderMutation, HttpRouteAction, UpstreamServer,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::{sleep, timeout},
};

use config_support::socket_endpoint;
use h1_wire::{Origin, STEP, Wire, find};

const WARMUP: u64 = 500;
const MEASURED: u64 = 2_000;
const REPLY: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 16\r\n\r\n0123456789abcdef";
const GET: &[u8] =
    b"GET /index.html HTTP/1.1\r\nHost: alloc.test\r\nUser-Agent: bench\r\nAccept: */*\r\n\r\n";
const POST: &[u8] =
    b"POST /submit HTTP/1.1\r\nHost: alloc.test\r\nContent-Length: 16\r\n\r\n0123456789abcdef";

struct Counts {
    allocations: u64,
}

/// One keep-alive connection that reads each response to its end.
struct Client(TcpStream);

impl Client {
    async fn send(&mut self, request: &[u8]) {
        self.0.write_all(request).await.expect("write request");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = timeout(STEP, self.0.read(&mut buffer))
                .await
                .expect("response in time")
                .expect("read response");
            assert!(count > 0, "the proxy closed the connection");
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = find(&bytes, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length = head.split("content-length: ").nth(1).map_or(0, |rest| {
                    rest.split("\r\n")
                        .next()
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or(0)
                });
                if bytes.len() >= end + 4 + length {
                    return;
                }
            }
        }
    }

    async fn send_many(&mut self, request: &[u8], count: u64) {
        for _ in 0..count {
            self.send(request).await;
        }
    }
}

/// The allocator calls the process has made so far: asks for a snapshot and waits for its line.
async fn snapshot(pid: u32, file: &Path) -> Counts {
    let before = fs::read_to_string(file).map_or(0, |text| text.lines().count());
    assert!(
        Command::new("kill")
            .args(["-USR1", &pid.to_string()])
            .status()
            .expect("kill")
            .success()
    );
    for _ in 0..200 {
        let text = fs::read_to_string(file).unwrap_or_default();
        if let Some(line) = text.lines().nth(before) {
            let allocations = line.split_whitespace().next().expect("allocation count");
            return Counts {
                allocations: allocations.parse().expect("allocation count"),
            };
        }
        sleep(Duration::from_millis(25)).await;
    }
    panic!("no allocation snapshot");
}

/// Allocator calls per request of `request` on a route configured by `edit` in front of `origins` more servers.
async fn per_request(
    request: &[u8],
    extra_origins: usize,
    edit: impl FnOnce(&mut ConfigDraft),
) -> u64 {
    let directory = TempDir::new().expect("snapshot directory");
    let file = directory.path().join("allocations.txt");
    let mut others = Vec::new();
    for _ in 0..extra_origins {
        others.push(Origin::start(REPLY).await);
    }
    let addresses: Vec<_> = others.iter().map(|origin| origin.address).collect();
    let wire = Wire::over_with_env(
        Origin::start(REPLY).await,
        &[(
            "OXIROUTE_INTERNAL_TEST_ALLOC_FILE",
            file.to_str().expect("utf-8 path"),
        )],
        |config| {
            for (n, address) in addresses.iter().enumerate() {
                config.upstream_pools[0].servers.push(UpstreamServer {
                    name: format!("extra-{n}"),
                    endpoint: socket_endpoint(*address),
                    max_connections: None,
                    dns_resolution: DnsResolutionPolicy::OnConnect,
                });
            }
            edit(config);
        },
    )
    .await;
    let mut client = Client(TcpStream::connect(wire.proxy).await.expect("connect"));
    client.send_many(request, WARMUP).await;
    let first = snapshot(wire.server.pid(), &file).await;
    client.send_many(request, MEASURED).await;
    let second = snapshot(wire.server.pid(), &file).await;
    wire.finish();
    second.allocations - first.allocations
}

fn rules(config: &mut ConfigDraft) {
    let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
    else {
        panic!("the route proxies");
    };
    let set = |name: &str, value| HttpRequestHeaderMutation::Set {
        name: name.into(),
        value,
    };
    policy.request_headers = vec![
        set("x-real-ip", HttpRequestHeaderValue::ClientIp),
        set(
            "x-forwarded-proto",
            HttpRequestHeaderValue::DownstreamScheme,
        ),
        set(
            "x-forwarded-host",
            HttpRequestHeaderValue::IncomingAuthority,
        ),
        set(
            "x-literal",
            HttpRequestHeaderValue::Literal {
                value: "fixed".into(),
            },
        ),
    ];
    policy.response_headers = vec![HttpResponseHeaderMutation::Set {
        name: "x-served-by".into(),
        value: "lane".into(),
        always: true,
    }];
}

fn fixed(config: &mut ConfigDraft) {
    config.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
        status: 200,
        body: "x".repeat(1024),
        headers: Vec::new(),
    };
}

fn check(shape: &str, calls: u64, ceiling: u64) {
    println!(
        "{shape}: {calls} allocator calls over {MEASURED} requests (ceiling {ceiling} per request)"
    );
    assert!(
        calls <= ceiling * MEASURED,
        "{shape}: {calls} allocator calls over {MEASURED} requests, ceiling {ceiling} per request"
    );
}

#[tokio::test]
async fn a_proxied_get_stays_within_its_allocation_ceiling() {
    check("proxied GET", per_request(GET, 0, |_| {}).await, 18);
}

#[tokio::test]
async fn a_proxied_get_with_header_rules_stays_within_its_allocation_ceiling() {
    check(
        "proxied GET with header rules",
        per_request(GET, 0, rules).await,
        18,
    );
}

#[tokio::test]
async fn a_small_post_stays_within_its_allocation_ceiling() {
    check("proxied POST", per_request(POST, 0, |_| {}).await, 18);
}

#[tokio::test]
async fn a_get_over_four_backends_stays_within_its_allocation_ceiling() {
    check(
        "proxied GET, 4 backends",
        per_request(GET, 3, |_| {}).await,
        18,
    );
}

#[tokio::test]
async fn a_fixed_response_stays_within_its_allocation_ceiling() {
    check("fixed response", per_request(GET, 0, fixed).await, 2);
}
