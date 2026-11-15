#![allow(clippy::pedantic)]

mod support;

use std::{
    io::{Read, Write},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use oxiroute_epoll_h1::{Failure, Timeouts};
use support::{Fixture, Knobs, eventually, test_config};

const SHORT: Duration = Duration::from_millis(150);

fn timeouts() -> Timeouts {
    Timeouts {
        first_request: Some(SHORT),
        keepalive: Some(SHORT),
        request: Some(SHORT),
        write: Some(SHORT),
    }
}

fn recorded() -> Knobs {
    Knobs {
        record: true,
        ..Knobs::default()
    }
}

fn failures(fixture: &Fixture) -> Vec<Option<Failure>> {
    fixture
        .outcomes()
        .iter()
        .map(|outcome| outcome.failure)
        .collect()
}

fn leases_released(fixture: &Fixture) {
    eventually("every upstream lease released", || {
        fixture.service.counters.live_leases.load(Ordering::SeqCst) == 0
    });
}

#[test]
fn a_connection_that_never_sends_a_request_is_closed_at_its_first_request_deadline() {
    let fixture = Fixture::with(test_config(), timeouts(), recorded());
    let started = Instant::now();
    let mut wire = fixture.wire();
    assert!(wire.ended());
    assert!(started.elapsed() >= SHORT && started.elapsed() < Duration::from_secs(3));
}

#[test]
fn an_idle_keepalive_connection_is_closed_at_its_keepalive_deadline() {
    let fixture = Fixture::with(test_config(), timeouts(), recorded());
    let mut wire = fixture.wire();
    wire.get("/size/3");
    assert_eq!(wire.reply().body.len(), 3);
    let started = Instant::now();
    assert!(wire.ended());
    assert!(started.elapsed() >= SHORT / 2 && started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_request_head_that_does_not_complete_is_closed_at_the_request_deadline() {
    let fixture = Fixture::with(test_config(), timeouts(), recorded());
    let mut wire = fixture.wire();
    wire.send(b"GET /size/3 HTTP/1.1\r\nHost: test.");
    let started = Instant::now();
    assert!(wire.ended());
    assert!(started.elapsed() >= SHORT / 2 && started.elapsed() < Duration::from_secs(3));
    assert!(fixture.outcomes().is_empty());
}

#[test]
fn an_origin_that_is_slow_to_answer_gets_a_504_at_the_header_deadline() {
    let knobs = Knobs {
        header: SHORT,
        ..recorded()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let mut wire = fixture.wire();
    wire.get("/delay/1500");
    let reply = wire.reply();
    assert_eq!(reply.status, 504);
    assert!(wire.ended());
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::HeaderTimeout)]
    });
    leases_released(&fixture);
}

#[test]
fn a_response_that_stalls_in_its_body_is_cut_at_the_body_deadline() {
    let knobs = Knobs {
        body: SHORT,
        ..recorded()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let mut wire = fixture.wire();
    wire.get("/stall");
    let mut seen = Vec::new();
    let _ = wire.stream.read_to_end(&mut seen);
    assert!(String::from_utf8_lossy(&seen).starts_with("HTTP/1.1 200 OK"));
    assert!(seen.ends_with(b"0123456789"));
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::BodyTimeout)]
    });
    assert_eq!(fixture.outcomes()[0].status, Some(200));
}

#[test]
fn the_total_deadline_of_a_request_ends_it_whatever_step_it_is_in() {
    let knobs = Knobs {
        total: Some(SHORT),
        ..recorded()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let mut wire = fixture.wire();
    wire.get("/delay/1500");
    assert_eq!(wire.reply().status, 504);
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::TotalTimeout)]
    });
}

#[test]
fn an_origin_that_does_not_accept_connections_gets_a_504_at_the_connect_deadline() {
    let blackhole = support::blackhole();
    let origin = support::Origin::start();
    let routes = vec![support::Route {
        host: "test.example",
        prefix: "/",
        origin: blackhole.addr,
    }];
    let knobs = Knobs {
        connect: SHORT,
        ..recorded()
    };
    let fixture = Fixture::with_routes(test_config(), Timeouts::default(), knobs, origin, routes);
    let mut wire = fixture.wire();
    wire.get("/x");
    assert_eq!(wire.reply().status, 504);
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::ConnectTimeout)]
    });
}

#[test]
fn a_refused_connection_is_a_502_without_waiting_for_a_deadline() {
    let origin = support::Origin::start();
    let routes = vec![support::Route {
        host: "test.example",
        prefix: "/",
        origin: support::closed_port(),
    }];
    let fixture = Fixture::with_routes(
        test_config(),
        Timeouts::default(),
        recorded(),
        origin,
        routes,
    );
    let started = Instant::now();
    let mut wire = fixture.wire();
    wire.get("/x");
    assert_eq!(wire.reply().status, 502);
    assert!(started.elapsed() < Duration::from_secs(2));
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::Connect)]
    });
}

#[test]
fn a_client_that_never_reads_is_cut_at_the_write_deadline() {
    let fixture = Fixture::with(test_config(), timeouts(), recorded());
    let mut wire = fixture.wire();
    wire.get("/size/40000000");
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::ClientTimeout)]
    });
    leases_released(&fixture);
    let mut sink = Vec::new();
    let _ = wire.stream.read_to_end(&mut sink);
    assert!(sink.len() < 40_000_000);
}

#[test]
fn a_slow_reader_gets_every_byte_through_bounded_buffers() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/size/9000000");
    std::thread::sleep(Duration::from_millis(300));
    let reply = wire.reply();
    assert_eq!(reply.body.len(), 9_000_000);
}

#[test]
fn an_early_answer_to_an_upload_reaches_a_client_that_is_still_sending() {
    let fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    let mut sender = wire.stream.try_clone().expect("clone");
    let head = b"POST /early HTTP/1.1\r\nHost: test.example\r\nContent-Length: 20000000\r\n\r\n";
    sender.write_all(head).expect("head");
    let writer = std::thread::spawn(move || {
        let block = vec![7_u8; 65536];
        for _ in 0..300 {
            if sender.write_all(&block).is_err() {
                break;
            }
        }
    });
    let reply = wire.reply();
    assert_eq!(reply.status, 413);
    assert_eq!(reply.header("connection").as_deref(), Some("close"));
    writer.join().expect("writer");
    eventually("the outcome", || fixture.outcomes().len() == 1);
    assert_eq!(fixture.outcomes()[0].status, Some(413));
}
