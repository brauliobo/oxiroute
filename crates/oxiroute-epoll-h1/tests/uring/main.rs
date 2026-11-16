#![allow(clippy::pedantic)]

//! What only the io_uring driver can get wrong: receive buffers that run out, sockets closed with operations in
//! flight, a slow client next to fast ones, and a ring that cannot be built. The rest of the engine suites run on
//! this driver with `EPOLL_H1_DRIVER=io_uring`.

#[path = "../support/mod.rs"]
mod support;

use std::{
    io::Read,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

use oxiroute_epoll_h1::{Config, Driver, Failure, Timeouts};
use support::{Fixture, Knobs, Wire, eventually, test_config};

fn on_uring(buffers: u16) -> Config {
    Config {
        driver: Driver::IoUring,
        uring: oxiroute_uring::Config {
            buffers,
            ..oxiroute_uring::Config::default()
        },
        ..test_config()
    }
}

fn recorded() -> Knobs {
    Knobs {
        record: true,
        ..Knobs::default()
    }
}

#[test]
fn a_ring_with_two_buffers_serves_many_connections_through_buffer_exhaustion() {
    let fixture = Fixture::with(on_uring(2), Timeouts::default(), Knobs::default());
    let mut wires: Vec<Wire> = (0..48).map(|_| fixture.wire()).collect();
    for round in 0..4 {
        for (index, wire) in wires.iter_mut().enumerate() {
            wire.get(&format!("/size/{}", 1000 + 1500 * index + round));
        }
        for (index, wire) in wires.iter_mut().enumerate() {
            let reply = wire.reply();
            assert_eq!(
                reply.body.len(),
                1000 + 1500 * index + round,
                "round {round} connection {index}: {}",
                reply.head
            );
        }
    }
    eventually("192 completed exchanges", || {
        fixture.service.counters.completed.load(Ordering::SeqCst) == 192
    });
}

#[test]
fn clients_that_leave_while_operations_are_in_flight_leave_nothing_behind() {
    let mut fixture = Fixture::with(on_uring(64), Timeouts::default(), recorded());
    let leavers: Vec<_> = (0..40)
        .map(|_| {
            let addr = fixture.addr;
            thread::spawn(move || {
                let mut wire = Wire::connect(addr);
                wire.get("/size/30000000");
                let mut first = [0_u8; 4096];
                wire.stream.read_exact(&mut first).expect("the first bytes");
            })
        })
        .collect();
    leavers
        .into_iter()
        .for_each(|leaver| leaver.join().expect("leaver"));
    eventually("every exchange ended", || fixture.outcomes().len() == 40);
    eventually("every admission released", || {
        fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst)
            == 0
    });
    eventually("every upstream lease released", || {
        fixture.service.counters.live_leases.load(Ordering::SeqCst) == 0
    });
    assert!(
        fixture
            .outcomes()
            .iter()
            .all(|outcome| outcome.failure.is_some())
    );
    let mut wire = fixture.wire();
    wire.get("/size/5");
    assert_eq!(wire.reply().body.len(), 5);
    fixture.drain_and_join();
}

#[test]
fn a_client_that_never_reads_does_not_slow_the_others_and_is_cut_at_the_write_deadline() {
    let timeouts = Timeouts {
        write: Some(Duration::from_millis(600)),
        ..Timeouts::default()
    };
    let fixture = Fixture::with(on_uring(256), timeouts, recorded());
    let mut stalled = fixture.wire();
    stalled.get("/size/40000000");
    let started = Instant::now();
    for _ in 0..50 {
        let mut wire = fixture.wire();
        wire.get("/size/20000");
        assert_eq!(wire.reply().body.len(), 20_000);
    }
    assert!(
        started.elapsed() < Duration::from_millis(550),
        "served at the pace of the fast clients"
    );
    eventually("the stalled client is cut", || {
        fixture
            .outcomes()
            .iter()
            .any(|outcome| outcome.failure == Some(Failure::ClientTimeout))
    });
    let mut sink = Vec::new();
    let _ = stalled.stream.read_to_end(&mut sink);
    assert!(sink.len() < 40_000_000);
}

#[test]
fn a_slow_client_receives_the_whole_last_response_before_the_connection_closes() {
    const SIZE: usize = 24_000_000;
    let fixture = Fixture::with(on_uring(64), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    wire.send(
        format!("GET /size/{SIZE} HTTP/1.1\r\nHost: test.example\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    );
    thread::sleep(Duration::from_millis(1500));
    let mut answer = Vec::new();
    wire.stream
        .read_to_end(&mut answer)
        .expect("a clean end of the stream");
    let head = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a head")
        + 4;
    assert_eq!(answer.len() - head, SIZE);
    eventually("the exchange completed", || fixture.outcomes().len() == 1);
    assert!(
        fixture
            .outcomes()
            .iter()
            .all(|outcome| outcome.failure.is_none())
    );
}

#[test]
fn several_threads_each_with_a_ring_serve_connections_together() {
    let config = Config {
        threads: 3,
        ..on_uring(128)
    };
    let fixture = Fixture::with(config, Timeouts::default(), Knobs::default());
    let addr = fixture.addr;
    let workers: Vec<_> = (0..4)
        .map(|worker| {
            thread::spawn(move || {
                let mut wires: Vec<Wire> = (0..50).map(|_| Wire::connect(addr)).collect();
                for round in 0..4 {
                    let size = 200 * (worker + 1) + round;
                    wires
                        .iter_mut()
                        .for_each(|wire| wire.get(&format!("/size/{size}")));
                    wires
                        .iter_mut()
                        .for_each(|wire| assert_eq!(wire.reply().body.len(), size));
                }
            })
        })
        .collect();
    workers
        .into_iter()
        .for_each(|worker| worker.join().expect("worker"));
    eventually("800 completed exchanges", || {
        fixture.service.counters.completed.load(Ordering::SeqCst) == 800
    });
}

#[test]
fn a_ring_that_cannot_be_built_stops_the_engine_from_starting() {
    let config = Config {
        driver: Driver::IoUring,
        uring: oxiroute_uring::Config {
            buffers: 3,
            ..oxiroute_uring::Config::default()
        },
        ..test_config()
    };
    let (sender, _handoffs) = std::sync::mpsc::channel();
    let (paused, _) = std::sync::mpsc::channel();
    let service = std::sync::Arc::new(support::Reference::new(
        Vec::new(),
        Knobs::default(),
        (sender, paused),
    ));
    let listener = oxiroute_epoll_h1::Listener::reuseport(
        "127.0.0.1:0".parse().expect("address"),
        1,
        service,
        Timeouts::default(),
    )
    .expect("listener");
    let error = oxiroute_epoll_h1::Engine::start(&config, vec![listener])
        .err()
        .expect("the engine does not start");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}
