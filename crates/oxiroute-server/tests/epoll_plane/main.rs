#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The epoll HTTP/1 data plane on the wire: what it serves itself (keep-alive, `Connection: close`, bodiless
//! responses, chunked and large bodies, pipelining, a failing origin) and what it hands to the tokio path before any
//! origin is contacted (a chunked request, an upgrade, `Expect`, a route with a TLS origin). The proxy is the real
//! binary in front of raw TCP origins; the debug line of the plane says which requests it served.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/h1_wire.rs"]
mod h1_wire;
#[path = "../support/process.rs"]
mod process_support;

use std::time::Duration;

use oxiroute_config::{
    ConfigDraft, ConnectionScheduling, HttpDataPlane, UpstreamTls, UpstreamTlsVerify, WorkerScaling,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

use fixture_support::fixture;
use h1_wire::{Origin, STEP, Wire, find, status};
use process_support::serves_on_epoll;

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const GET: &[u8] = b"GET /r HTTP/1.1\r\nHost: example.test\r\n\r\n";
const SERVED_ON_EPOLL: &str = "served by the epoll data plane";
const HEAD_REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n";
const CHUNKED: &[u8] =
    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n3\r\nabc\r\n0\r\n\r\n";
const FIRST: &[u8] = b"GET /one HTTP/1.1\r\nHost: example.test\r\n\r\n";
const SECOND: &[u8] = b"GET /two HTTP/1.1\r\nHost: example.test\r\n\r\n";
const SLOW: &[h1_wire::Step] = &[h1_wire::Step::Slow(Duration::from_secs(30), OK)];
const SWITCH: &[u8] =
    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";

fn logged(config: &mut ConfigDraft) {
    config.log_filter = Some("oxiroute::epoll=debug".into());
}

fn big_bodies(config: &mut ConfigDraft) {
    logged(config);
    config.http_services[0].max_request_body_bytes = Some(64 * 1024 * 1024);
}

fn expected_on_epoll(requests: usize) -> usize {
    if serves_on_epoll() { requests } else { 0 }
}

fn served(wire: Wire) -> usize {
    let output = wire.server.shutdown_output();
    output
        .lines()
        .filter(|line| line.contains(SERVED_ON_EPOLL))
        .count()
}

async fn read_response(stream: &mut TcpStream) -> (String, Vec<u8>) {
    read_answer(stream, false).await
}

async fn read_answer(stream: &mut TcpStream, head_request: bool) -> (String, Vec<u8>) {
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
    let lower = head.to_ascii_lowercase();
    let length = lower
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let bodiless =
        head_request || status(&head).contains(" 204 ") || status(&head).contains(" 304 ");
    let length = if bodiless { 0 } else { length };
    while bytes.len() < head_end + length {
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response body")
            .expect("read");
        assert_ne!(count, 0, "closed inside the body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    (head, bytes[head_end..head_end + length].to_vec())
}

async fn closed_by_the_proxy(stream: &mut TcpStream) -> bool {
    let mut buffer = [0_u8; 64];
    matches!(timeout(STEP, stream.read(&mut buffer)).await, Ok(Ok(0)))
}

fn leaked(bytes: Vec<u8>) -> &'static [u8] {
    Box::leak(bytes.into_boxed_slice())
}

#[tokio::test]
async fn keep_alive_requests_are_served_on_one_connection() {
    let wire = Wire::over(Origin::start(OK).await, logged).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    for _ in 0..3 {
        client.write_all(GET).await.unwrap();
        let (head, body) = read_response(&mut client).await;
        assert_eq!(status(&head), "HTTP/1.1 200 OK", "{head}");
        assert!(head.contains("Connection: keep-alive"), "{head}");
        assert_eq!(body, b"ok");
    }
    assert_eq!(wire.origin.requests().len(), 3);
    assert_eq!(served(wire), expected_on_epoll(3));
}

#[tokio::test]
async fn connection_close_ends_the_connection_after_the_answer() {
    let wire = Wire::over(Origin::start(OK).await, logged).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client
        .write_all(b"GET /r HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let (head, body) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 200 OK", "{head}");
    assert!(head.contains("Connection: close"), "{head}");
    assert_eq!(body, b"ok");
    assert!(closed_by_the_proxy(&mut client).await);
    assert_eq!(served(wire), expected_on_epoll(1));
}

#[tokio::test]
async fn head_and_bodiless_statuses_have_no_body_and_keep_the_connection() {
    let wire = Wire::over(Origin::start(HEAD_REPLY).await, logged).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    for _ in 0..2 {
        client
            .write_all(b"HEAD /r HTTP/1.1\r\nHost: example.test\r\n\r\n")
            .await
            .unwrap();
        let (head, body) = read_answer(&mut client, true).await;
        assert_eq!(status(&head), "HTTP/1.1 200 OK", "{head}");
        assert!(head.contains("Content-Length: 10"), "{head}");
        assert_eq!(body, Vec::<u8>::new());
    }
    assert_eq!(served(wire), expected_on_epoll(2));

    for (reply, line) in [
        (
            &b"HTTP/1.1 204 No Content\r\nX-A: 1\r\n\r\n"[..],
            "HTTP/1.1 204 No Content",
        ),
        (
            &b"HTTP/1.1 304 Not Modified\r\nContent-Length: 10\r\n\r\n"[..],
            "HTTP/1.1 304 Not Modified",
        ),
    ] {
        let wire = Wire::over(Origin::start(reply).await, logged).await;
        let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
        for _ in 0..2 {
            client.write_all(GET).await.unwrap();
            let (head, body) = read_response(&mut client).await;
            assert_eq!(status(&head), line, "{head}");
            assert_eq!(body, Vec::<u8>::new());
        }
        assert_eq!(served(wire), expected_on_epoll(2));
    }
}

#[tokio::test]
async fn chunked_and_large_bodies_arrive_intact() {
    let large = vec![b'x'; 300_000];
    let sized = leaked(
        [
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", large.len()).as_bytes(),
            &large,
        ]
        .concat(),
    );
    let wire = Wire::over(Origin::start(sized).await, logged).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(GET).await.unwrap();
    let (_, body) = read_response(&mut client).await;
    assert_eq!(body, large);
    assert_eq!(served(wire), expected_on_epoll(1));

    let wire = Wire::over(Origin::start(CHUNKED).await, logged).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(GET).await.unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    while find(&bytes, b"0\r\n\r\n").is_none() {
        let count = timeout(STEP, client.read(&mut buffer))
            .await
            .expect("chunked response")
            .expect("read");
        assert_ne!(count, 0, "closed inside the chunked body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("transfer-encoding: chunked") || text.contains("Transfer-Encoding: chunked"),
        "{text}"
    );
    assert!(
        text.ends_with("5\r\nhello\r\n3\r\nabc\r\n0\r\n\r\n"),
        "{text}"
    );
    assert_eq!(served(wire), expected_on_epoll(1));
}

#[tokio::test]
async fn request_bodies_of_every_size_reach_the_origin() {
    let wire = Wire::over(Origin::start(OK).await, big_bodies).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    let sizes = [1, 1_000, 16_383, 16_384, 70_000, 400_000];
    for size in sizes {
        let body = vec![b'b'; size];
        client
            .write_all(
                format!(
                    "POST /up HTTP/1.1\r\nHost: example.test\r\nContent-Length: {size}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        client.write_all(&body).await.unwrap();
        let (head, answer) = read_response(&mut client).await;
        assert_eq!(status(&head), "HTTP/1.1 200 OK", "size {size}: {head}");
        assert_eq!(answer, b"ok");
    }
    let received = wire.origin.requests.lock().unwrap().len();
    assert_eq!(received, sizes.len());
    assert_eq!(served(wire), expected_on_epoll(sizes.len()));
}

#[tokio::test]
async fn pipelined_requests_are_answered_in_order() {
    if !serves_on_epoll() {
        return;
    }
    let wire = Wire::over(Origin::start(OK).await, logged).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(&[FIRST, SECOND].concat()).await.unwrap();
    for _ in 0..2 {
        let (head, body) = read_response(&mut client).await;
        assert_eq!(status(&head), "HTTP/1.1 200 OK", "{head}");
        assert_eq!(body, b"ok");
    }
    let requests = wire.origin.requests();
    assert!(requests[0].starts_with("GET /one "), "{requests:?}");
    assert!(requests[1].starts_with("GET /two "), "{requests:?}");
    assert_eq!(served(wire), expected_on_epoll(2));
}

#[tokio::test]
async fn an_origin_that_refuses_connections_is_a_bad_gateway() {
    let wire = Wire::over(Origin::start(OK).await, |config| {
        logged(config);
        config.upstream_pools[0].servers[0].endpoint =
            config_support::loopback_endpoint(process_support::reserve_tcp_address().port());
    })
    .await;
    let response = wire.exchange(GET).await;
    assert_eq!(status(&response), "HTTP/1.1 502 Bad Gateway", "{response}");
    assert_eq!(served(wire), expected_on_epoll(1));
}

#[tokio::test]
async fn a_request_that_the_origin_never_answers_times_out_as_a_gateway_timeout() {
    let wire = Wire::over(Origin::scripted(SLOW).await, |config| {
        logged(config);
        config.http_services[0].upstream_io_timeout_ms = 300;
    })
    .await;
    let response = wire.exchange(GET).await;
    assert!(
        status(&response) == "HTTP/1.1 504 Gateway Timeout"
            || status(&response) == "HTTP/1.1 502 Bad Gateway",
        "{response}"
    );
    assert_eq!(served(wire), expected_on_epoll(1));
}

#[tokio::test]
async fn a_chunked_request_is_handed_to_the_tokio_path() {
    let wire = Wire::over(Origin::start(OK).await, big_bodies).await;
    let response = wire
        .exchange(
            b"POST /r HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert_eq!(wire.origin.requests().len(), 1);
    assert_eq!(served(wire), 0);
}

#[tokio::test]
async fn an_expect_continue_request_is_handed_to_the_tokio_path() {
    let wire = Wire::over(Origin::start(OK).await, big_bodies).await;
    let response = wire
        .exchange(
            b"POST /r HTTP/1.1\r\nHost: example.test\r\nContent-Length: 3\r\nExpect: 100-continue\r\n\r\nabc",
        )
        .await;
    assert!(response.contains("200 OK"), "{response}");
    assert_eq!(served(wire), 0);
}

#[tokio::test]
async fn an_upgrade_request_is_handed_to_the_tokio_path() {
    let wire = Wire::over(Origin::start(SWITCH).await, logged).await;
    let response = wire
        .exchange(
            b"GET /ws HTTP/1.1\r\nHost: example.test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .await;
    assert_eq!(
        status(&response),
        "HTTP/1.1 101 Switching Protocols",
        "{response}"
    );
    assert_eq!(served(wire), 0);
}

#[tokio::test]
async fn a_route_with_a_tls_origin_is_served_by_the_tokio_path() {
    let origin = Origin::start_tls(OK, false).await;
    let wire = Wire::over(origin, |config| {
        logged(config);
        config.upstream_pools[0].tls = Some(UpstreamTls {
            server_name: Some("origin.example.test".into()),
            verify: UpstreamTlsVerify::Full,
            ca_certificate_path: Some(fixture("ca-a.pem")),
        });
    })
    .await;
    let response = wire.exchange(GET).await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert_eq!(served(wire), 0);
}

#[tokio::test]
async fn a_listener_that_chooses_the_tokio_plane_is_served_by_it() {
    let wire = Wire::over(Origin::start(OK).await, |config| {
        logged(config);
        config.http_data_plane = Some(oxiroute_config::HttpDataPlane::Tokio);
    })
    .await;
    let response = wire.exchange(GET).await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert_eq!(served(wire), 0);
}

fn scaled_dynamically(config: &mut ConfigDraft) {
    logged(config);
    config.http_data_plane = None;
    config.listener_workers = Some(2);
    config.connection_scheduling = Some(ConnectionScheduling::AgedPerWorker);
    config.worker_scaling = Some(WorkerScaling::Dynamic);
}

fn aged(config: &mut ConfigDraft) {
    logged(config);
    config.http_data_plane = None;
    config.listener_workers = Some(2);
    config.connection_scheduling = Some(ConnectionScheduling::AgedPerWorker);
    config.connection_age_ms = Some(50);
}

async fn served_by_the_epoll_plane(edit: fn(&mut ConfigDraft)) {
    let wire = Wire::over(Origin::start(OK).await, edit).await;
    let response = wire.exchange(GET).await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert_eq!(served(wire), expected_on_epoll(1));
}

fn explicitly_on_epoll(edit: fn(&mut ConfigDraft), config: &mut ConfigDraft) {
    edit(config);
    config.http_data_plane = Some(HttpDataPlane::Epoll);
}

#[tokio::test]
async fn a_listener_with_dynamic_worker_scaling_is_served_by_the_epoll_plane() {
    served_by_the_epoll_plane(scaled_dynamically).await;
}

#[tokio::test]
async fn a_listener_with_aged_scheduling_and_a_connection_age_is_served_by_the_epoll_plane() {
    served_by_the_epoll_plane(aged).await;
}

#[tokio::test]
async fn choosing_epoll_with_dynamic_scaling_is_served_by_it() {
    let wire = Wire::over(Origin::start(OK).await, |config| {
        explicitly_on_epoll(scaled_dynamically, config);
    })
    .await;
    let response = wire.exchange(GET).await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert_eq!(served(wire), expected_on_epoll(1));
}
