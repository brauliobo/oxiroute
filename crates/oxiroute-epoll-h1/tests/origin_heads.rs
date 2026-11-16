#![allow(clippy::pedantic)]

mod support;

use std::time::{Duration, Instant};

use oxiroute_epoll_h1::{Failure, Timeouts};
use support::{Fixture, Knobs, eventually, test_config};

fn recorded() -> Fixture {
    let knobs = Knobs {
        record: true,
        ..Knobs::default()
    };
    Fixture::with(test_config(), Timeouts::default(), knobs)
}

fn assert_failed_with(fixture: &Fixture, failure: Failure) {
    eventually("the outcome", || {
        fixture
            .outcomes()
            .last()
            .is_some_and(|outcome| outcome.failure == Some(failure))
    });
}

#[test]
fn an_interim_response_is_forwarded_and_the_final_one_follows_on_the_same_connection() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    for _ in 0..2 {
        wire.get("/interim");
        let interim = wire.head();
        assert_eq!(interim.status, 103);
        assert_eq!(
            interim.header("link").as_deref(),
            Some("</a.css>; rel=preload")
        );
        assert!(interim.header("x-request-id").is_some());
        let reply = wire.reply();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"ok");
    }
}

#[test]
fn an_interim_response_reaches_the_client_before_the_origin_sends_the_final_one() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    let started = Instant::now();
    wire.get("/interim-late/600");
    assert_eq!(wire.head().status, 103);
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    let reply = wire.reply();
    assert_eq!(reply.status, 200);
    assert!(started.elapsed() >= Duration::from_millis(600));
}

#[test]
fn an_origin_that_closes_after_an_interim_response_is_a_bad_gateway_after_it() {
    let fixture = recorded();
    let mut wire = fixture.wire();
    wire.get("/interim-only");
    assert_eq!(wire.head().status, 103);
    assert_eq!(wire.reply().status, 502);
    assert!(wire.ended());
    assert_failed_with(&fixture, Failure::UpstreamClosed);
}

#[test]
fn a_switch_of_protocols_nobody_asked_for_is_a_bad_gateway() {
    let fixture = recorded();
    let mut wire = fixture.wire();
    wire.get("/switch");
    assert_eq!(wire.reply().status, 502);
    assert!(wire.ended());
    assert_failed_with(&fixture, Failure::BadResponse);
}

#[test]
fn a_head_in_the_lenient_shape_is_relayed_in_the_canonical_one() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/lenient");
    let reply = wire.reply();
    assert_eq!(reply.status, 200);
    assert!(
        reply.head.starts_with("HTTP/1.1 200 \n"),
        "{:?}",
        reply.head
    );
    assert_eq!(reply.header("x-a").as_deref(), Some("one two"));
    assert_eq!(reply.header("x-b").as_deref(), Some("b"));
    assert_eq!(reply.header("upgrade"), None);
    assert_eq!(reply.body, b"ok");
    wire.get("/size/2");
    assert_eq!(wire.reply().body.len(), 2);
}

#[test]
fn a_head_beyond_the_old_sixteen_kib_limit_is_relayed_and_one_beyond_a_mebibyte_is_not() {
    let fixture = recorded();
    let mut wire = fixture.wire();
    wire.get("/largehead");
    let reply = wire.reply();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("x-big").map(|value| value.len()), Some(30_000));
    wire.get("/size/2");
    assert_eq!(wire.reply().body.len(), 2);
    let mut wire = fixture.wire();
    wire.get("/hugehead");
    assert_eq!(wire.reply().status, 502);
    assert_failed_with(&fixture, Failure::ResponseHeadTooLarge);
}
