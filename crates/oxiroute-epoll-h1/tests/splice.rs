#![allow(clippy::pedantic)]

mod support;

use std::{
    io::Read,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use oxiroute_epoll_h1::{Config, Failure, Splice, SpliceUse, Timeouts};
use support::{Fixture, Knobs, Wire, eventually, sized, test_config};

fn config(pipes: usize) -> Config {
    Config {
        splice: Some(Splice {
            threshold: 16 * 1024,
            pipes,
            pipe_bytes: 64 * 1024,
        }),
        ..test_config()
    }
}

fn fixture(pipes: usize) -> Fixture {
    Fixture::with(config(pipes), Timeouts::default(), Knobs::default())
}

fn failed(fixture: &Fixture) -> usize {
    fixture.service.counters.failed.load(Ordering::SeqCst)
}

fn fetch(wire: &mut Wire, path: &str) -> Vec<u8> {
    wire.get(path);
    let reply = wire.reply();
    assert_eq!(reply.status, 200);
    reply.body
}

fn read_until_end(wire: &mut Wire) -> usize {
    let mut total = 0;
    let mut chunk = vec![0_u8; 65536];
    while let Ok(count) = wire.stream.read(&mut chunk) {
        if count == 0 {
            break;
        }
        total += count;
    }
    total
}

#[test]
fn bodies_from_one_byte_to_sixteen_mebibytes_arrive_intact_over_one_upstream_connection() {
    let fixture = fixture(8);
    let mut wire = fixture.wire();
    for len in [
        1, 1023, 16_000, 16_384, 20_000, 32_768, 65_536, 100_001, 1_048_576, 4_194_304, 16_777_216,
    ] {
        assert!(
            fetch(&mut wire, &format!("/size/{len}")) == sized(len),
            "{len}"
        );
    }
    assert_eq!(fixture.origin.accepted(), 1);
}

#[test]
fn spliced_and_small_responses_alternate_on_pooled_connections() {
    let fixture = fixture(8);
    let mut wire = fixture.wire();
    for round in 0..30 {
        let len = 70_000 + round;
        assert!(fetch(&mut wire, &format!("/size/{len}")) == sized(len));
        assert_eq!(fetch(&mut wire, "/size/10"), sized(10));
    }
    assert_eq!(fixture.origin.accepted(), 1);
}

#[test]
fn pipelined_requests_for_spliced_bodies_are_answered_in_order() {
    let fixture = fixture(8);
    let mut wire = fixture.wire();
    let request = |path: &str| format!("GET {path} HTTP/1.1\r\nHost: test.example\r\n\r\n");
    wire.send(
        format!(
            "{}{}{}",
            request("/size/300000"),
            request("/size/5"),
            request("/size/200000")
        )
        .as_bytes(),
    );
    for len in [300_000, 5, 200_000] {
        assert!(wire.reply().body == sized(len));
    }
}

#[test]
fn a_slow_client_stalls_only_its_own_exchange_and_gets_every_byte() {
    let fixture = fixture(8);
    let mut slow = fixture.wire();
    slow.get("/size/16000000");
    std::thread::sleep(Duration::from_millis(400));
    let mut quick = fixture.wire();
    let started = Instant::now();
    assert_eq!(fetch(&mut quick, "/size/100"), sized(100));
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(slow.reply().body == sized(16_000_000));
}

#[test]
fn a_slow_origin_delivers_the_body_in_pieces() {
    let fixture = fixture(8);
    let mut wire = fixture.wire();
    assert!(fetch(&mut wire, "/slow/500000") == sized(500_000));
    assert!(fetch(&mut wire, "/size/100000") == sized(100_000));
    assert_eq!(fixture.origin.accepted(), 1);
}

#[test]
fn an_origin_reset_mid_body_ends_the_response_and_gives_the_pipe_back() {
    let knobs = Knobs {
        record: true,
        ..Knobs::default()
    };
    let fixture = Fixture::with(config(1), Timeouts::default(), knobs);
    for _ in 0..5 {
        let mut wire = fixture.wire();
        wire.get("/cut/400000");
        assert!(read_until_end(&mut wire) < 400_000);
    }
    eventually("five failed exchanges", || failed(&fixture) == 5);
    let outcomes = fixture.outcomes();
    assert_eq!(outcomes.len(), 5);
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.failure == Some(Failure::UpstreamClosed))
    );
    let mut wire = fixture.wire();
    assert!(fetch(&mut wire, "/size/400000") == sized(400_000));
}

#[test]
fn a_client_reset_mid_body_leaves_nothing_in_the_next_response() {
    let fixture = fixture(1);
    for round in 0..20 {
        let mut gone = fixture.wire();
        gone.get("/size/24000000");
        let mut chunk = vec![0_u8; 65536];
        gone.stream.read_exact(&mut chunk).expect("first bytes");
        rustix::net::sockopt::set_socket_linger(&gone.stream, Some(Duration::ZERO))
            .expect("linger");
        drop(gone);
        let len = 100_000 + round;
        assert!(fetch(&mut fixture.wire(), &format!("/size/{len}")) == sized(len));
    }
    eventually("twenty reset exchanges", || failed(&fixture) == 20);
}

#[test]
fn an_origin_that_stalls_mid_body_hits_the_body_deadline() {
    let knobs = Knobs {
        body: Duration::from_millis(300),
        record: true,
        ..Knobs::default()
    };
    let fixture = Fixture::with(config(8), Timeouts::default(), knobs);
    let mut wire = fixture.wire();
    wire.get("/hang/400000");
    let started = Instant::now();
    assert!(read_until_end(&mut wire) < 400_000);
    assert!(started.elapsed() < Duration::from_secs(2));
    eventually("a body timeout", || {
        fixture
            .outcomes()
            .iter()
            .any(|outcome| outcome.failure == Some(Failure::BodyTimeout))
    });
}

#[test]
fn bytes_after_the_declared_body_are_never_forwarded_nor_left_in_a_pooled_connection() {
    let fixture = fixture(8);
    let mut wire = fixture.wire();
    assert!(fetch(&mut wire, "/extra/100000") == sized(100_000));
    assert_eq!(fetch(&mut wire, "/size/100"), sized(100));
    assert_eq!(fixture.origin.accepted(), 2);
}

fn recording() -> Knobs {
    Knobs {
        record: true,
        ..Knobs::default()
    }
}

fn uses(fixture: &Fixture, wanted: SpliceUse) -> usize {
    let outcomes = fixture.outcomes();
    outcomes
        .iter()
        .filter(|outcome| outcome.splice == wanted)
        .count()
}

#[test]
fn pipe_exhaustion_sends_the_remaining_bodies_through_the_buffers_and_is_counted() {
    let fixture = Fixture::with(config(1), Timeouts::default(), recording());
    let mut wires: Vec<Wire> = (0..4).map(|_| fixture.wire()).collect();
    for (index, wire) in wires.iter_mut().enumerate() {
        wire.get(&format!("/size/{}", 24_000_000 + index));
    }
    for (index, wire) in wires.iter_mut().enumerate() {
        assert!(wire.reply().body == sized(24_000_000 + index));
    }
    eventually("four recorded exchanges", || fixture.outcomes().len() == 4);
    assert!(uses(&fixture, SpliceUse::Spliced) >= 1);
    assert!(uses(&fixture, SpliceUse::PoolFull) >= 1);
    assert_eq!(uses(&fixture, SpliceUse::OpenFailed), 0);
    assert_eq!(
        uses(&fixture, SpliceUse::Spliced) + uses(&fixture, SpliceUse::PoolFull),
        4
    );
}

#[test]
fn bodies_below_the_threshold_do_not_count_as_splice_fallbacks() {
    let fixture = Fixture::with(config(1), Timeouts::default(), recording());
    let mut wire = fixture.wire();
    assert_eq!(fetch(&mut wire, "/size/1000"), sized(1000));
    eventually("a recorded exchange", || fixture.outcomes().len() == 1);
    assert_eq!(uses(&fixture, SpliceUse::Unused), 1);
}

#[test]
fn a_disabled_splice_keeps_the_buffered_path_and_counts_nothing() {
    let config = Config {
        splice: None,
        ..test_config()
    };
    let fixture = Fixture::with(config, Timeouts::default(), recording());
    let mut wire = fixture.wire();
    assert!(fetch(&mut wire, "/size/3000000") == sized(3_000_000));
    eventually("a recorded exchange", || fixture.outcomes().len() == 1);
    assert_eq!(uses(&fixture, SpliceUse::Unused), 1);
}
