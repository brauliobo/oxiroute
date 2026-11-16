use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use crate::{
    common::{active, connections, echo, open, pack, relocations, scaled},
    support::{Fixture, Knobs, eventually},
};

const THREADS: usize = 4;
const RELEASE: Duration = Duration::from_millis(150);

fn started() -> Fixture {
    Fixture::on_shared_listener(scaled(THREADS, 1, RELEASE), Knobs::default())
}

#[test]
fn the_engine_starts_with_every_thread_active_and_connections_spread() {
    let fixture = started();
    let wires = open(&fixture, 32);

    let stats = fixture.engine().scaling_stats().expect("scaling");

    assert_eq!((stats.workers, stats.active), (THREADS, THREADS));
    assert!(connections(&fixture).iter().all(|held| *held > 0));
    drop(wires);
}

#[test]
fn light_traffic_packs_the_connections_on_one_thread_and_every_answer_stays_intact() {
    let fixture = started();
    let mut wires = open(&fixture, 32);

    pack(&fixture, &mut wires, THREADS);

    assert!(relocations(&fixture) > 0);
    wires
        .iter_mut()
        .enumerate()
        .for_each(|(id, wire)| echo(wire, id, 4096));
}

#[test]
fn a_thread_that_left_the_active_set_takes_no_new_connection() {
    let fixture = started();
    let mut wires = open(&fixture, 8);
    pack(&fixture, &mut wires, THREADS);

    let more = open(&fixture, 16);

    assert_eq!(connections(&fixture), [24, 0, 0, 0]);
    drop(more);
}

#[test]
fn load_activates_the_threads_again_and_idle_connections_follow() {
    let fixture = started();
    let mut wires = open(&fixture, 32);
    pack(&fixture, &mut wires, THREADS);
    let before = relocations(&fixture);

    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        let (done, fixture) = (&done, &fixture);
        for chunk in wires.chunks_mut(4) {
            scope.spawn(move || {
                let mut id = 0;
                while !done.load(Ordering::Relaxed) {
                    chunk.iter_mut().for_each(|wire| echo(wire, id, 512));
                    id += 1;
                }
            });
        }
        eventually("more threads to be activated", || active(fixture) >= 2);
        eventually("connections to move to the activated threads", || {
            connections(fixture)
                .iter()
                .filter(|held| **held > 0)
                .count()
                >= 2
        });
        done.store(true, Ordering::Relaxed);
    });

    assert!(relocations(&fixture) > before);
}

#[test]
fn a_scaled_engine_with_as_many_threads_as_the_minimum_never_scales() {
    let fixture = Fixture::on_shared_listener(scaled(2, 2, RELEASE), Knobs::default());
    let wires = open(&fixture, 8);

    assert!(fixture.engine().scaling_stats().is_none());
    assert_eq!(connections(&fixture).iter().sum::<usize>(), 8);
    drop(wires);
}
