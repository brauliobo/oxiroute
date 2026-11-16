#![allow(clippy::pedantic)]

#[path = "../support/mod.rs"]
mod support;

use std::{sync::atomic::Ordering, time::Duration};

use oxiroute_epoll_h1::{Config, Timeouts};
use support::{Fixture, Knobs, Wire, eventually, test_config};

const CONNECTIONS: usize = 400;

fn fixture(config: Config) -> Fixture {
    Fixture::with(config, Timeouts::default(), Knobs::default())
}

fn serve_round(wires: &mut [Wire], path: &str, expected: usize) {
    for wire in wires.iter_mut() {
        wire.get(path);
    }
    for wire in wires.iter_mut() {
        assert_eq!(wire.reply().body.len(), expected);
    }
}

#[test]
fn a_burst_of_requests_in_flight_at_once_is_served_and_the_connections_keep_going() {
    let fixture = fixture(test_config());
    let mut wires: Vec<Wire> = (0..CONNECTIONS).map(|_| fixture.wire()).collect();
    serve_round(&mut wires, "/delay/150", 4);
    serve_round(&mut wires, "/size/300", 300);
    serve_round(&mut wires, "/delay/20", 4);
    eventually("every exchange completed", || {
        fixture.service.counters.completed.load(Ordering::SeqCst) == 3 * CONNECTIONS
    });
}

#[test]
fn responses_larger_than_the_first_buffers_cross_in_full_after_a_burst() {
    let fixture = fixture(test_config());
    let mut wires: Vec<Wire> = (0..CONNECTIONS).map(|_| fixture.wire()).collect();
    serve_round(&mut wires, "/delay/100", 4);
    for size in [1_500, 5_000, 70_000, 300_000] {
        serve_round(&mut wires[..50], &format!("/size/{size}"), size);
    }
    serve_round(&mut wires, "/size/10", 10);
}

#[test]
fn a_request_larger_than_the_first_buffer_is_assembled_and_answered() {
    let fixture = fixture(test_config());
    let mut wire = fixture.wire();
    let padding = "x".repeat(3_000);
    wire.send(
        format!("GET /size/7 HTTP/1.1\r\nHost: test.example\r\nX-Pad: {padding}\r\n\r\n")
            .as_bytes(),
    );
    assert_eq!(wire.reply().body.len(), 7);
    let body = vec![b'q'; 9_000];
    wire.send(
        format!(
            "POST /echo HTTP/1.1\r\nHost: test.example\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    wire.send(&body);
    assert_eq!(wire.reply().body, body);
}

#[test]
fn a_tiny_event_batch_still_serves_every_connection() {
    let config = Config {
        events: 3,
        ..test_config()
    };
    let fixture = fixture(config);
    let mut wires: Vec<Wire> = (0..CONNECTIONS).map(|_| fixture.wire()).collect();
    for _ in 0..3 {
        serve_round(&mut wires, "/size/200", 200);
    }
}

#[test]
fn a_connection_streaming_a_large_body_does_not_hold_back_the_others() {
    let fixture = fixture(test_config());
    let mut bulk = fixture.wire();
    bulk.get("/size/16000000");
    let mut others: Vec<Wire> = (0..100).map(|_| fixture.wire()).collect();
    let started = std::time::Instant::now();
    serve_round(&mut others, "/size/50", 50);
    serve_round(&mut others, "/size/50", 50);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(bulk.reply().body.len(), 16_000_000);
}

fn paced(starts_per_pass: usize) -> Fixture {
    fixture(Config {
        starts_per_pass,
        ..test_config()
    })
}

#[test]
fn a_burst_is_served_in_full_when_a_pass_starts_one_exchange() {
    let fixture = paced(1);
    let mut wires: Vec<Wire> = (0..CONNECTIONS).map(|_| fixture.wire()).collect();
    for _ in 0..3 {
        serve_round(&mut wires, "/size/200", 200);
    }
    eventually("every exchange completed", || {
        fixture.service.counters.completed.load(Ordering::SeqCst) == 3 * CONNECTIONS
    });
}

#[test]
fn slow_exchanges_do_not_hold_back_the_starts_of_other_connections() {
    let fixture = paced(1);
    let mut slow: Vec<Wire> = (0..100).map(|_| fixture.wire()).collect();
    for wire in &mut slow {
        wire.get("/delay/600");
    }
    eventually("every slow exchange started and none ended", || {
        let counters = &fixture.service.counters;
        counters.live_leases.load(Ordering::SeqCst) == slow.len()
            && counters.completed.load(Ordering::SeqCst) == 0
    });
    let mut quick = fixture.wire();
    quick.get("/size/10");
    assert_eq!(quick.reply().body.len(), 10);
    for wire in &mut slow {
        assert_eq!(wire.reply().body.len(), 4);
    }
}

#[test]
fn connections_waiting_to_start_are_released_when_they_close() {
    let fixture = paced(1);
    let mut wires: Vec<Wire> = (0..CONNECTIONS).map(|_| fixture.wire()).collect();
    for wire in &mut wires {
        wire.get("/size/20");
    }
    wires.truncate(CONNECTIONS / 2);
    for wire in &mut wires {
        assert_eq!(wire.reply().body.len(), 20);
    }
    drop(wires);
    let counters = &fixture.service.counters;
    eventually("every connection was released", || {
        counters.live_admissions.load(Ordering::SeqCst) == 0
    });
}
