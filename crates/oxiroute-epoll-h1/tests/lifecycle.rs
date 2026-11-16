#![allow(clippy::pedantic)]

mod support;

use std::{
    io::Read as _,
    net::TcpStream,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

use oxiroute_epoll_h1::{Failure, Timeouts};
use support::{Fixture, KILL, Knobs, MOVE, Wire, eventually, test_config};

fn recorded() -> Knobs {
    Knobs {
        record: true,
        ..Knobs::default()
    }
}

#[test]
fn origin_failures_are_answered_with_a_502_and_the_client_connection_ends() {
    let fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    for (path, failure) in [
        ("/closed", Failure::UpstreamClosed),
        ("/garbage", Failure::BadResponse),
        ("/hugehead", Failure::ResponseHeadTooLarge),
    ] {
        let mut wire = fixture.wire();
        wire.get(path);
        let reply = wire.reply();
        assert_eq!(reply.status, 502, "{path}");
        assert!(wire.ended(), "{path}");
        eventually("the outcome", || {
            fixture
                .outcomes()
                .last()
                .is_some_and(|outcome| outcome.failure == Some(failure))
        });
    }
}

#[test]
fn a_retryable_status_is_retried_and_only_the_final_answer_reaches_the_client() {
    let knobs = Knobs {
        retries: 1,
        retry_status: Some(503),
        ..recorded()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let mut wire = fixture.wire();
    wire.get("/flaky");
    let reply = wire.reply();
    assert_eq!(reply.status, 200);
    eventually("the outcome", || fixture.outcomes().len() == 1);
    assert_eq!(fixture.outcomes()[0].attempts, 2);
    assert_eq!(fixture.outcomes()[0].failure, None);
    wire.get("/size/2");
    assert_eq!(wire.reply().body.len(), 2);
}

#[test]
fn a_request_too_large_to_replay_is_never_offered_a_retry() {
    let knobs = Knobs {
        retries: 1,
        retry_status: Some(503),
        ..recorded()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let spent = || {
        fixture
            .service
            .counters
            .spent_retries
            .load(Ordering::SeqCst)
    };
    let mut wire = fixture.wire();
    let body = vec![b'x'; 40_000];
    wire.send(b"POST /flaky HTTP/1.1\r\nHost: test.example\r\nContent-Length: 40000\r\n\r\n");
    wire.send(&body);
    assert_eq!(wire.reply().status, 503);
    assert_eq!(spent(), 0);
    fixture.origin.flaky_left.store(1, Ordering::SeqCst);
    let mut small = fixture.wire();
    small.get("/flaky");
    assert_eq!(small.reply().status, 200);
    assert_eq!(spent(), 1);
}

#[test]
fn a_retry_the_service_allows_replays_a_request_that_was_buffered() {
    let knobs = Knobs {
        retries: 2,
        ..recorded()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let mut wire = fixture.wire();
    wire.send(b"POST /closed HTTP/1.1\r\nHost: test.example\r\nContent-Length: 3\r\n\r\nabc");
    assert_eq!(wire.reply().status, 502);
    eventually("the outcome", || fixture.outcomes().len() == 1);
    assert_eq!(fixture.outcomes()[0].attempts, 3);
}

#[test]
fn outcomes_count_the_bytes_that_moved_each_way() {
    let fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    wire.send(b"POST /echo HTTP/1.1\r\nHost: test.example\r\nContent-Length: 6\r\n\r\nabcdef");
    assert_eq!(wire.reply().body, b"abcdef");
    eventually("the outcome", || fixture.outcomes().len() == 1);
    let outcome = fixture.outcomes()[0];
    assert_eq!(
        (outcome.status, outcome.received, outcome.sent),
        (Some(200), 6, 6)
    );
    assert!(outcome.connect.is_some() && outcome.header.is_some() && outcome.keepalive);
}

#[test]
fn many_connections_over_several_threads_are_all_served() {
    let config = oxiroute_epoll_h1::Config {
        threads: 3,
        ..test_config()
    };
    let fixture = Fixture::with(config, Timeouts::default(), Knobs::default());
    let addr = fixture.addr;
    let workers: Vec<_> = (0..4)
        .map(|worker| {
            thread::spawn(move || {
                let mut wires: Vec<Wire> = (0..60).map(|_| Wire::connect(addr)).collect();
                for round in 0..5 {
                    let size = 100 * (worker + 1) + round;
                    for wire in &mut wires {
                        wire.get(&format!("/size/{size}"));
                    }
                    for wire in &mut wires {
                        assert_eq!(wire.reply().body.len(), size);
                    }
                }
            })
        })
        .collect();
    workers
        .into_iter()
        .for_each(|worker| worker.join().expect("worker"));
    eventually("1200 completed exchanges", || {
        fixture.service.counters.completed.load(Ordering::SeqCst) == 1200
    });
    assert_eq!(
        fixture.service.counters.admitted.load(Ordering::SeqCst),
        240
    );
}

#[test]
fn a_connection_the_service_does_not_admit_is_closed_and_the_others_go_on() {
    let knobs = Knobs {
        admit_limit: Some(4),
        ..Knobs::default()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let mut wires: Vec<Wire> = (0..4).map(|_| fixture.wire()).collect();
    for wire in &mut wires {
        wire.get("/size/1");
        assert_eq!(wire.reply().body.len(), 1);
    }
    let mut extra = fixture.wire();
    assert!(extra.ended());
    wires[0].get("/size/2");
    assert_eq!(wires[0].reply().body.len(), 2);
}

#[test]
fn a_request_that_finishes_during_the_drain_closes_its_connection() {
    let mut fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    wire.get("/slowbody/400");
    let mut seen = Vec::new();
    while !seen.ends_with(b"ab") {
        let mut chunk = [0_u8; 256];
        let count = wire
            .stream
            .read(&mut chunk)
            .expect("the head and its first bytes");
        assert_ne!(count, 0, "closed inside the response");
        seen.extend_from_slice(&chunk[..count]);
    }
    let drained = Instant::now();
    fixture.engine().drain();
    let mut rest = Vec::new();
    wire.stream
        .read_to_end(&mut rest)
        .expect("the engine closes the connection once the response is whole");
    assert_eq!(rest, b"cd");
    assert!(
        drained.elapsed() < Duration::from_secs(2),
        "the connection waited for the drain deadline"
    );
    fixture.drain_and_join();
}

#[test]
fn draining_closes_idle_connections_finishes_requests_in_flight_and_stops_accepting() {
    let mut fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut idle = fixture.wire();
    idle.get("/size/1");
    assert_eq!(idle.reply().body.len(), 1);
    let mut busy = fixture.wire();
    busy.get("/delay/300");
    eventually("the request reached the origin", || {
        fixture.origin.heads.lock().expect("heads").len() == 2
    });
    fixture.engine().drain();
    assert!(idle.ended());
    let reply = busy.reply();
    assert_eq!((reply.status, reply.text().as_str()), (200, "late"));
    assert_eq!(reply.header("connection").as_deref(), Some("close"));
    assert!(busy.ended());
    fixture.drain_and_join();
    assert!(TcpStream::connect(fixture.addr).is_err());
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

#[test]
fn stopping_ends_requests_in_flight_and_reports_them() {
    let mut fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut busy = fixture.wire();
    busy.get("/delay/2000");
    eventually("the request reached the origin", || {
        !fixture.origin.heads.lock().expect("heads").is_empty()
    });
    fixture.stop();
    assert!(busy.ended());
    assert_eq!(fixture.outcomes().len(), 1);
    assert_eq!(fixture.outcomes()[0].failure, Some(Failure::Shutdown));
    assert_eq!(
        fixture.service.counters.live_leases.load(Ordering::SeqCst),
        0
    );
}

#[test]
fn a_client_that_leaves_mid_exchange_releases_its_connection() {
    let fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    wire.get("/delay/400");
    eventually("the request reached the origin", || {
        !fixture.origin.heads.lock().expect("heads").is_empty()
    });
    drop(wire);
    eventually("the outcome", || fixture.outcomes().len() == 1);
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
fn idle_upstream_connections_are_closed_at_their_idle_deadline() {
    let knobs = Knobs {
        idle_upstream: Duration::from_millis(100),
        ..Knobs::default()
    };
    let fixture = Fixture::with(test_config(), Timeouts::default(), knobs);
    let mut wire = fixture.wire();
    wire.get("/size/1");
    assert_eq!(wire.reply().body.len(), 1);
    eventually("the idle upstream closed", || {
        fixture.service.counters.live_leases.load(Ordering::SeqCst) == 0
    });
    wire.get("/size/1");
    assert_eq!(wire.reply().body.len(), 1);
    assert_eq!(fixture.origin.accepted(), 2);
}

#[test]
fn a_paused_engine_keeps_serving_its_connections_and_leaves_new_ones_queued_until_it_resumes() {
    let fixture = Fixture::start();
    let mut served = fixture.wire();
    served.get("/size/2");
    assert_eq!(served.reply().status, 200);
    fixture.engine().pause();
    let mut queued = fixture.wire();
    queued.get("/size/2");
    assert!(queued.silent_for(Duration::from_millis(300)));
    served.get("/size/2");
    assert_eq!(served.reply().status, 200);
    fixture.engine().resume();
    assert_eq!(queued.reply().status, 200);
}

#[test]
fn an_engine_started_paused_accepts_nothing_until_it_resumes() {
    let fixture = Fixture::paused();
    let mut queued = fixture.wire();
    queued.get("/size/2");
    assert!(queued.silent_for(Duration::from_millis(300)));
    fixture.engine().resume();
    assert_eq!(queued.reply().status, 200);
}

fn request_at_the_origin(fixture: &Fixture, requests: usize) {
    eventually("the request reached the origin", || {
        fixture.origin.heads.lock().expect("heads").len() == requests
    });
}

#[test]
fn a_cancelled_request_waiting_on_its_origin_is_sent_again() {
    let fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    wire.get("/delay/800");
    request_at_the_origin(&fixture, 1);
    fixture.service.cancel(MOVE);
    request_at_the_origin(&fixture, 2);
    let reply = wire.reply();
    assert_eq!((reply.status, reply.text().as_str()), (200, "late"));
    eventually("the outcome", || fixture.outcomes().len() == 1);
    assert_eq!(fixture.outcomes()[0].attempts, 2);
    assert_eq!(fixture.outcomes()[0].failure, None);
    assert_eq!(fixture.origin.accepted(), 2);
}

#[test]
fn a_killed_request_waiting_on_its_origin_is_answered_and_its_origin_connection_closed() {
    let fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    wire.get("/delay/3000");
    request_at_the_origin(&fixture, 1);
    fixture.service.cancel(KILL);
    assert_eq!(wire.reply().status, 502);
    assert!(wire.ended());
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::Cancelled)]
    });
    eventually("every upstream lease released", || {
        fixture.service.counters.live_leases.load(Ordering::SeqCst) == 0
    });
}

#[test]
fn a_killed_request_whose_response_started_ends_its_client_connection() {
    let fixture = Fixture::with(test_config(), Timeouts::default(), recorded());
    let mut wire = fixture.wire();
    wire.get("/stall");
    let mut seen = Vec::new();
    while !seen.ends_with(b"0123456789") {
        let mut chunk = [0_u8; 256];
        let count = wire
            .stream
            .read(&mut chunk)
            .expect("the head and the first bytes");
        assert_ne!(count, 0, "closed before the response started");
        seen.extend_from_slice(&chunk[..count]);
    }
    fixture.service.cancel(KILL);
    let mut rest = Vec::new();
    wire.stream
        .read_to_end(&mut rest)
        .expect("the engine ends the connection");
    assert!(rest.is_empty());
    eventually("the outcome", || {
        failures(&fixture) == [Some(Failure::Cancelled)]
    });
}

fn failures(fixture: &Fixture) -> Vec<Option<Failure>> {
    fixture
        .outcomes()
        .iter()
        .map(|outcome| outcome.failure)
        .collect()
}
