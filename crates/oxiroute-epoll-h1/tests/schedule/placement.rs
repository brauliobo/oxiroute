use oxiroute_epoll_h1::Age;

use crate::{
    common::{HOUR, aged, connections, echo, open, plain},
    support::{Fixture, Knobs},
};

const THREADS: usize = 4;
const CONNECTIONS: usize = 64;

fn spread(held: &[usize]) -> usize {
    held.iter().max().unwrap() - held.iter().min().unwrap()
}

#[test]
fn connections_are_placed_on_the_threads_with_the_fewest_open_ones() {
    let fixture = Fixture::on_shared_listener(aged(THREADS, Age::Fixed(HOUR)), Knobs::default());
    let wires = open(&fixture, CONNECTIONS);

    let held = connections(&fixture);

    assert_eq!(held.iter().sum::<usize>(), CONNECTIONS, "{held:?}");
    assert!(spread(&held) <= 2, "{held:?}");
    drop(wires);
}

#[test]
fn per_thread_sockets_hashed_unevenly_by_the_kernel_are_still_balanced() {
    let fixture = Fixture::with(
        aged(THREADS, Age::Fixed(HOUR)),
        Default::default(),
        Knobs::default(),
    );
    let wires = open(&fixture, CONNECTIONS);

    let held = connections(&fixture);

    assert_eq!(held.iter().sum::<usize>(), CONNECTIONS, "{held:?}");
    assert!(spread(&held) <= 2, "{held:?}");
    drop(wires);
}

#[test]
fn a_thread_gives_its_slot_back_when_its_connection_ends() {
    let fixture = Fixture::on_shared_listener(aged(THREADS, Age::Fixed(HOUR)), Knobs::default());
    let mut wires = open(&fixture, THREADS * 4);
    wires.drain(..THREADS * 2);
    crate::support::eventually("the closed connections to be given back", || {
        connections(&fixture).iter().sum::<usize>() == THREADS * 2
    });
    let mut more = open(&fixture, THREADS * 2);
    more.iter_mut()
        .enumerate()
        .for_each(|(id, wire)| echo(wire, id, 100));

    let held = connections(&fixture);

    assert_eq!(held.iter().sum::<usize>(), THREADS * 4, "{held:?}");
    assert!(spread(&held) <= 2, "{held:?}");
}

#[test]
fn without_scheduling_the_engine_counts_nothing() {
    let fixture = Fixture::on_shared_listener(plain(THREADS), Knobs::default());
    let wires = open(&fixture, 8);

    assert!(connections(&fixture).is_empty());
    assert!(fixture.engine().scaling_stats().is_none());
    drop(wires);
}

#[test]
fn a_rejected_connection_takes_no_slot() {
    let knobs = Knobs {
        admit_limit: Some(2),
        ..Knobs::default()
    };
    let fixture = Fixture::on_shared_listener(aged(THREADS, Age::Fixed(HOUR)), knobs);
    let wires = open(&fixture, 2);
    let mut refused = fixture.wire();
    assert!(refused.ended());

    assert_eq!(connections(&fixture).iter().sum::<usize>(), 2);
    drop(wires);
}
