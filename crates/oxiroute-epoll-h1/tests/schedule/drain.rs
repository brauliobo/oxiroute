use std::{sync::atomic::Ordering, thread, time::Duration};

use oxiroute_epoll_h1::Config;

use crate::{
    common::{active, connections, echo, open, pack, scaled},
    support::{Fixture, Knobs, eventually},
};

const THREADS: usize = 4;
const RELEASE: Duration = Duration::from_millis(100);

fn packed() -> (Fixture, Vec<crate::support::Wire>) {
    packed_with(scaled(THREADS, 1, RELEASE))
}

fn packed_with(config: Config) -> (Fixture, Vec<crate::support::Wire>) {
    let fixture = Fixture::on_shared_listener(config, Knobs::default());
    let mut wires = open(&fixture, 16);
    pack(&fixture, &mut wires, THREADS);
    (fixture, wires)
}

#[test]
fn draining_a_scaled_engine_finishes_the_request_in_flight_and_closes_the_idle_connections() {
    let (mut fixture, mut wires) = packed();
    let mut busy = wires.pop().expect("a connection");
    busy.get("/delay/300");
    thread::sleep(Duration::from_millis(100));

    fixture.drain_and_join();

    let reply = busy.reply();
    assert_eq!((reply.status, reply.text().as_str()), (200, "late"));
    assert!(busy.ended());
    assert!(wires.iter_mut().all(|wire| wire.ended()));
    assert_eq!(
        fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst),
        0
    );
}

#[test]
fn the_scaling_of_an_engine_ends_with_it() {
    let config = scaled(THREADS, 1, RELEASE);
    let scale = config
        .scheduling
        .as_ref()
        .and_then(|scheduling| scheduling.scale.as_ref());
    let name = scale.expect("scaling").name.clone();
    let (mut fixture, wires) = packed_with(config);
    let listed = || {
        let stats = oxiroute_worker_scaling::process_stats();
        stats.iter().any(|(listed, _)| *listed == name)
    };
    assert!(listed());

    drop(wires);
    fixture.stop();

    assert!(!listed());
}

#[test]
fn pausing_and_resuming_with_released_threads_neither_hangs_nor_loses_a_connection() {
    let (fixture, mut wires) = packed();
    assert_eq!(active(&fixture), 1);

    let engine = fixture.engine();
    thread::scope(|scope| {
        let paused = scope.spawn(|| engine.pause());
        eventually("the pause to take hold", || paused.is_finished());
    });
    let mut waiting = fixture.wire();
    waiting.get("/while-paused");
    assert!(waiting.silent_for(Duration::from_millis(300)));

    thread::scope(|scope| {
        let resumed = scope.spawn(|| engine.resume());
        eventually("the resume to take hold", || resumed.is_finished());
    });

    assert_eq!(waiting.reply().status, 200);
    wires
        .iter_mut()
        .enumerate()
        .for_each(|(id, wire)| echo(wire, id, 512));
    assert_eq!(connections(&fixture).iter().sum::<usize>(), 17);
}
