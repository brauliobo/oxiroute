#![allow(clippy::pedantic)]

mod support;

use std::time::Duration;

use support::{Fixture, Knobs, eventually, test_config};

#[test]
fn keepalive_requests_share_one_client_and_one_upstream_connection() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    for _ in 0..50 {
        wire.get("/size/1000");
        let reply = wire.reply();
        assert_eq!((reply.status, reply.body.len()), (200, 1000));
        assert_eq!(reply.header("connection").as_deref(), Some("keep-alive"));
    }
    assert_eq!(fixture.origin.accepted(), 1);
    eventually("fifty completed exchanges", || {
        fixture
            .service
            .counters
            .completed
            .load(std::sync::atomic::Ordering::SeqCst)
            == 50
    });
}

#[test]
fn the_head_to_the_origin_and_the_response_to_the_client_carry_the_edits_of_the_service() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.send(b"GET /info HTTP/1.1\r\nHost: test.example\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n");
    let reply = wire.reply();
    let seen = reply.text();
    assert!(seen.contains("x-request-id: "), "{seen}");
    assert!(!seen.to_ascii_lowercase().contains("connection:"), "{seen}");
    let id = reply.header("x-request-id").expect("a stamped id");
    assert_eq!(id.len(), 16);
    assert!(
        reply
            .header("date")
            .is_some_and(|date| date.ends_with("GMT"))
    );
    assert_eq!(
        reply.header("content-length").as_deref(),
        Some(&*seen.len().to_string())
    );
}

#[test]
fn a_client_request_id_is_forwarded_as_it_is() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.send(b"GET /info HTTP/1.1\r\nHost: test.example\r\nX-Request-Id: abc123\r\n\r\n");
    let reply = wire.reply();
    let seen = reply.text();
    assert_eq!(
        seen.to_ascii_lowercase().matches("x-request-id").count(),
        1,
        "{seen}"
    );
    assert!(seen.contains("x-request-id: abc123"), "{seen}");
}

#[test]
fn large_response_and_request_bodies_stream_through_bounded_buffers() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/size/3000000");
    let reply = wire.reply();
    assert_eq!(reply.body.len(), 3_000_000);
    assert_eq!(reply.body[22..25], *b"wab");
    let upload: Vec<u8> = (0..2_500_000_u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let mut head = format!(
        "POST /echo HTTP/1.1\r\nHost: test.example\r\nContent-Length: {}\r\n\r\n",
        upload.len()
    )
    .into_bytes();
    head.extend_from_slice(&upload[..1000]);
    wire.send(&head);
    wire.send(&upload[1000..]);
    let echoed = wire.reply();
    assert_eq!(echoed.body, upload);
    wire.get("/size/10");
    assert_eq!(wire.reply().body.len(), 10);
}

#[test]
fn chunked_responses_are_streamed_with_their_trailers() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/chunked/40");
    let reply = wire.reply();
    let expected: String = (0..40).map(|index| format!("chunk-{index};")).collect();
    assert_eq!(reply.text(), expected);
    assert!(String::from_utf8_lossy(&reply.raw_body).contains("X-Trailer: yes"));
    assert_eq!(
        reply.header("transfer-encoding").as_deref(),
        Some("chunked")
    );
    wire.get("/size/5");
    assert_eq!(wire.reply().body.len(), 5);
    assert_eq!(fixture.origin.accepted(), 1);
}

#[test]
fn a_response_delimited_by_the_close_is_chunked_for_the_client_and_its_upstream_is_not_reused() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/until-close");
    let reply = wire.reply();
    assert_eq!(reply.text(), "part-one;part-two;");
    assert_eq!(
        reply.header("transfer-encoding").as_deref(),
        Some("chunked")
    );
    assert_eq!(reply.header("connection").as_deref(), Some("keep-alive"));
    wire.get("/size/5");
    assert_eq!(wire.reply().body.len(), 5);
    assert_eq!(fixture.origin.accepted(), 2);
}

#[test]
fn head_204_and_304_responses_have_no_body_and_the_connection_goes_on() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.send(b"HEAD /head HTTP/1.1\r\nHost: test.example\r\n\r\n");
    let head = wire.reply_to(true);
    assert_eq!(
        (head.status, head.header("content-length").as_deref()),
        (200, Some("5"))
    );
    for status in [204, 304] {
        wire.get(&format!("/status/{status}"));
        let reply = wire.reply();
        assert_eq!(reply.status, status);
        assert!(reply.body.is_empty());
        assert_eq!(reply.header("x-kind").as_deref(), Some("empty"));
    }
    wire.get("/size/7");
    assert_eq!(wire.reply().body.len(), 7);
    assert_eq!(fixture.origin.accepted(), 1);
}

#[test]
fn connection_close_ends_the_connection_after_the_response() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.send(b"GET /size/20 HTTP/1.1\r\nHost: test.example\r\nConnection: close\r\n\r\n");
    let reply = wire.reply();
    assert_eq!(reply.body.len(), 20);
    assert_eq!(reply.header("connection").as_deref(), Some("close"));
    assert!(wire.ended());
}

#[test]
fn pipelined_requests_are_answered_in_order_one_at_a_time() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    let request = |size: usize| format!("GET /size/{size} HTTP/1.1\r\nHost: test.example\r\n\r\n");
    let all = format!("{}{}{}", request(11), request(22), request(33));
    wire.send(all.as_bytes());
    let sizes: Vec<usize> = (0..3).map(|_| wire.reply().body.len()).collect();
    assert_eq!(sizes, [11, 22, 33]);
}

#[test]
fn a_request_split_into_single_bytes_is_assembled() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    let request = b"POST /echo HTTP/1.1\r\nHost: test.example\r\nContent-Length: 5\r\n\r\nhello";
    for byte in request {
        wire.send(&[*byte]);
    }
    assert_eq!(wire.reply().body, b"hello");
}

#[test]
fn an_origin_that_closes_a_connection_it_called_keepalive_does_not_break_the_next_request() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    for _ in 0..3 {
        wire.get("/once");
        assert_eq!(wire.reply().body, b"once");
    }
    assert_eq!(fixture.origin.accepted(), 3);
}

#[test]
fn a_pooled_upstream_the_origin_closed_unseen_is_replaced_when_the_service_retries() {
    let knobs = Knobs {
        pause_in_route: Some(Duration::from_millis(200)),
        retries: 1,
        ..Knobs::default()
    };
    let fixture = Fixture::with(test_config(), oxiroute_epoll_h1::Timeouts::default(), knobs);
    let mut first = fixture.wire();
    first.get("/size/3");
    assert_eq!(first.reply().body.len(), 3);
    assert_eq!(fixture.origin.accepted(), 1);
    let mut victim = fixture.wire();
    victim.get("/pause");
    fixture.paused.recv().expect("the engine entered the route");
    fixture.origin.close_connections();
    let reply = victim.reply();
    assert_eq!((reply.status, reply.body.as_slice()), (200, &b"ok"[..]));
    assert_eq!(fixture.origin.accepted(), 2);
}
