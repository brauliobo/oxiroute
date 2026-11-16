use std::{
    net::SocketAddr,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

use crate::{
    common::{active, connections, echo, open, scaled},
    support::{Fixture, Knobs, Wire, eventually},
};

const RELEASE: Duration = Duration::from_millis(100);

fn started(threads: usize) -> Fixture {
    Fixture::on_shared_listener(scaled(threads, 1, RELEASE), Knobs::default())
}

#[test]
fn a_connection_with_a_request_in_flight_stays_on_its_thread_until_the_answer_is_sent() {
    let fixture = started(2);
    let mut wires = open(&fixture, 8);
    for wire in &mut wires {
        wire.get("/delay/1500");
    }

    eventually("the second thread to be released", || active(&fixture) == 1);
    thread::sleep(Duration::from_millis(100));

    let held = connections(&fixture);
    assert_eq!(held, [4, 4], "{held:?}");
    for wire in &mut wires {
        let reply = wire.reply();
        assert_eq!((reply.status, reply.text().as_str()), (200, "late"));
    }
    eventually("the answered connections to move", || {
        connections(&fixture) == [8, 0]
    });
    for (id, wire) in wires.iter_mut().enumerate() {
        echo(wire, id, 2048);
    }
}

#[test]
fn large_bodies_arrive_intact_before_and_after_a_connection_moves() {
    let fixture = started(2);
    let mut wires = open(&fixture, 6);
    for (id, wire) in wires.iter_mut().enumerate() {
        echo(wire, id, 300_000);
    }

    eventually("every connection to move to the first thread", || {
        wires
            .iter_mut()
            .enumerate()
            .for_each(|(id, wire)| echo(wire, id, 1000 + id));
        connections(&fixture) == [6, 0]
    });
    for (id, wire) in wires.iter_mut().enumerate() {
        echo(wire, id, 300_000);
    }
}

#[test]
fn a_request_sent_while_the_connection_moves_is_answered_once() {
    let fixture = started(2);
    let mut wires = open(&fixture, 8);
    let started = Instant::now();
    let mut answered = 0;
    while started.elapsed() < Duration::from_secs(3) {
        for (id, wire) in wires.iter_mut().enumerate() {
            echo(wire, id, 8);
            answered += 1;
        }
        thread::sleep(Duration::from_millis(30));
    }

    assert_eq!(
        fixture.service.counters.completed.load(Ordering::SeqCst),
        answered + 8
    );
    assert_eq!(fixture.origin.heads.lock().unwrap().len(), answered + 8);
}

fn churn(addr: SocketAddr, clients: usize, rounds: usize) -> usize {
    thread::scope(|scope| {
        let handles = (0..clients)
            .map(|client| {
                scope.spawn(move || {
                    let mut sent = 0;
                    for round in 0..rounds {
                        let mut wire = Wire::connect(addr);
                        for request in 0..3 {
                            echo(&mut wire, client * 1000 + round * 3 + request, 64 + round);
                            sent += 1;
                            if (client + round + request) % 4 == 0 {
                                thread::sleep(Duration::from_millis(20));
                            }
                        }
                    }
                    sent
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum()
    })
}

#[test]
fn no_connection_or_request_is_lost_while_connections_come_go_and_move() {
    let fixture = started(4);
    let held = open(&fixture, 16);

    let sent = churn(fixture.addr, 8, 40);

    drop(held);
    eventually("every connection to be released", || {
        connections(&fixture).iter().sum::<usize>() == 0
            && fixture
                .service
                .counters
                .live_admissions
                .load(Ordering::SeqCst)
                == 0
    });
    let counters = &fixture.service.counters;
    assert_eq!(counters.completed.load(Ordering::SeqCst), sent + 16);
    assert_eq!(counters.failed.load(Ordering::SeqCst), 0);
}
