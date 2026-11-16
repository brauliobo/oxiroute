#![allow(clippy::pedantic)]

mod support;

use std::{
    io::{Read, Write},
    sync::atomic::Ordering,
    time::Duration,
};

use oxiroute_epoll_h1::{Config, Timeouts};
use support::{Fixture, Knobs, eventually, test_config};

const CHUNK: usize = 16 * 1024;

fn handed_off(fixture: &Fixture) -> oxiroute_epoll_h1::Handoff<support::Guard> {
    fixture
        .handoffs
        .recv_timeout(Duration::from_secs(5))
        .expect("a handoff")
}

fn assert_handoff(request: &[u8], fixture: &Fixture) {
    let mut wire = fixture.wire();
    wire.send(request);
    let mut handoff = handed_off(fixture);
    assert_eq!(handoff.prefix, request);
    assert_eq!(handoff.listener, 0);
    assert_eq!(
        handoff.peer,
        wire.stream.local_addr().expect("client address")
    );
    handoff.stream.set_nonblocking(false).expect("blocking");
    handoff
        .stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
        .expect("answer");
    assert_eq!(wire.reply().body, b"hi");
}

#[test]
fn requests_the_engine_cannot_serve_are_handed_off_with_the_bytes_already_read() {
    let fixture = Fixture::start();
    for request in [
        &b"POST /x HTTP/1.1\r\nHost: test.example\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n"[..],
        b"GET /x HTTP/1.1\r\nHost: test.example\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        b"POST /x HTTP/1.1\r\nHost: test.example\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n",
        b"GET /x HTTP/1.0\r\nHost: test.example\r\n\r\n",
        b"GET /x HTTP/1.1\r\n\r\n",
        b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: test.example\r\nConnection: te, close\r\n\r\n",
    ] {
        assert_handoff(request, &fixture);
    }
}

#[test]
fn requests_the_service_does_not_route_are_handed_off() {
    let fixture = Fixture::start();
    assert_handoff(
        b"GET /handoff/here HTTP/1.1\r\nHost: test.example\r\n\r\n",
        &fixture,
    );
    assert_handoff(b"GET /x HTTP/1.1\r\nHost: other.example\r\n\r\n", &fixture);
    assert_handoff(
        b"GET /x HTTP/1.1\r\nHost: test.example\r\nKeep-Alive: 5\r\n\r\n",
        &fixture,
    );
}

#[test]
fn a_head_over_the_limit_is_handed_off_with_everything_read() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    let request = format!(
        "GET /x HTTP/1.1\r\nHost: test.example\r\nX-Big: {}\r\n\r\n",
        "a".repeat(12_000)
    );
    wire.send(request.as_bytes());
    let handoff = handed_off(&fixture);
    assert!(handoff.prefix.len() >= 8 * 1024);
    assert!(request.as_bytes().starts_with(&handoff.prefix));
}

fn chunked_upload(head_padding: usize, body: usize) -> Vec<u8> {
    let mut request = format!(
        "POST /x HTTP/1.1\r\nHost: test.example\r\nTransfer-Encoding: chunked\r\nX-Pad: {}\r\n\r\n{body:x}\r\n",
        "a".repeat(head_padding)
    )
    .into_bytes();
    request.extend(std::iter::repeat_n(b'b', body));
    request.extend_from_slice(b"\r\n0\r\n\r\n");
    request
}

fn what_the_tokio_path_receives(fixture: &Fixture, request: &[u8]) -> (usize, Vec<u8>) {
    let mut wire = fixture.wire();
    wire.send(request);
    let mut handoff = handed_off(fixture);
    let prefix = handoff.prefix.len();
    let mut received = handoff.prefix;
    let mut rest = vec![0_u8; request.len() - prefix];
    handoff.stream.set_nonblocking(false).expect("blocking");
    handoff
        .stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    handoff
        .stream
        .read_exact(&mut rest)
        .expect("the rest of the request stays on the socket");
    received.extend(rest);
    (prefix, received)
}

#[test]
fn a_handed_off_request_with_a_body_over_a_chunk_arrives_whole() {
    let fixture = Fixture::start();
    for body in [CHUNK - 1, CHUNK, CHUNK + 1, 4 * CHUNK] {
        let request = chunked_upload(0, body);
        let (_, received) = what_the_tokio_path_receives(&fixture, &request);
        assert!(
            received == request,
            "{body} body bytes were not handed off intact"
        );
    }
}

#[test]
fn a_prefix_over_a_chunk_is_handed_off_byte_for_byte() {
    let config = Config {
        head_limit: 64 * 1024,
        request_buffer: 64 * 1024,
        ..test_config()
    };
    let fixture = Fixture::with(config, Timeouts::default(), Knobs::default());
    let request = chunked_upload(CHUNK + 4_000, 3 * CHUNK);
    let (prefix, received) = what_the_tokio_path_receives(&fixture, &request);
    assert!(
        prefix > CHUNK,
        "the engine read {prefix} bytes before the handoff"
    );
    assert!(
        received == request,
        "the handed-off request differs from the one sent"
    );
}

#[test]
fn a_connection_that_served_requests_is_handed_off_with_its_admission() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/size/4");
    assert_eq!(wire.reply().body.len(), 4);
    wire.get("/size/4");
    assert_eq!(wire.reply().body.len(), 4);
    wire.send(b"GET /handoff HTTP/1.1\r\nHost: test.example\r\n\r\n");
    let mut handoff = handed_off(&fixture);
    assert_eq!(
        fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst),
        1
    );
    handoff.stream.set_nonblocking(false).expect("blocking");
    handoff
        .stream
        .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
        .expect("answer");
    assert_eq!(wire.reply().status, 204);
    wire.send(b"x");
    let mut seen = [0_u8; 1];
    handoff
        .stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    handoff
        .stream
        .read_exact(&mut seen)
        .expect("the connection carries on after the handoff");
    assert_eq!(&seen, b"x");
    drop(handoff);
    eventually("the admission released", || {
        fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst)
            == 0
    });
}

#[test]
fn a_handed_off_request_leaves_no_state_in_the_engine() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.send(b"POST /x HTTP/1.1\r\nHost: test.example\r\nTransfer-Encoding: chunked\r\n\r\n");
    let handoff = handed_off(&fixture);
    drop(handoff);
    assert!(wire.ended());
    assert_eq!(
        fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        fixture.service.counters.live_leases.load(Ordering::SeqCst),
        0
    );
}
