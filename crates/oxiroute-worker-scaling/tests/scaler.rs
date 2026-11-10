//! The controller thread against workers whose busy time and connections the test sets.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use oxiroute_worker_scaling::{Policy, Scaling, Workers};

struct Fake {
    busy_ns: Vec<AtomicU64>,
    connections: Vec<AtomicUsize>,
}

impl Fake {
    fn new(workers: usize) -> Arc<Self> {
        Arc::new(Self {
            busy_ns: (0..workers).map(|_| AtomicU64::new(0)).collect(),
            connections: (0..workers).map(|_| AtomicUsize::new(0)).collect(),
        })
    }
}

impl Workers for Fake {
    fn len(&self) -> usize {
        self.busy_ns.len()
    }

    fn busy_nanos(&self, worker: usize) -> u64 {
        self.busy_ns[worker].load(Ordering::Relaxed)
    }

    fn connections(&self, worker: usize) -> usize {
        self.connections[worker].load(Ordering::Relaxed)
    }
}

fn policy() -> Policy {
    Policy {
        min_active: 1,
        target_busy_permille: 500,
        release_delay: Duration::from_millis(100),
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn idle_connections_are_packed_and_busy_ones_spread_again() {
    let workers = Fake::new(4);
    workers.connections[0].store(8, Ordering::Relaxed);
    let scaling = Arc::new(Scaling::new("scaler-test", policy(), 4));
    scaling.start(workers.clone());
    assert_eq!(scaling.active(), 4);
    wait_for("the light load to be packed", || scaling.active() == 1);
    assert_eq!(scaling.stats().workers, 4);

    // Worker 0 is now busy all the time: the busy counter grows as fast as the clock.
    let done = Arc::new(AtomicBool::new(false));
    let busy = thread::spawn({
        let (workers, done) = (workers.clone(), done.clone());
        move || {
            let start = Instant::now();
            while !done.load(Ordering::Relaxed) {
                let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap();
                workers.busy_ns[0].store(nanos, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(2));
            }
        }
    });
    wait_for("a second worker to be activated", || scaling.active() >= 2);
    done.store(true, Ordering::Relaxed);
    busy.join().unwrap();
    scaling.stop();
}

#[test]
fn the_threshold_for_shedding_follows_the_connection_count() {
    let workers = Fake::new(4);
    for connections in &workers.connections {
        connections.store(10, Ordering::Relaxed);
    }
    let held = Policy {
        min_active: 4,
        ..policy()
    };
    let scaling = Arc::new(Scaling::new("scaler-test-threshold", held, 4));
    scaling.start(workers);
    // 40 connections over four active workers: 10 each plus the slack of two.
    wait_for("the threshold", || scaling.rebalance_above() == 12);
    scaling.stop();
}

#[test]
fn a_controller_with_no_connections_sleeps_until_one_is_placed() {
    let workers = Fake::new(2);
    let scaling = Arc::new(Scaling::new("scaler-test-sleep", policy(), 2));
    scaling.start(workers.clone());
    wait_for("the idle controller to sleep", || scaling.sleeping());
    workers.connections[0].store(1, Ordering::Relaxed);
    scaling.connection_placed();
    wait_for("the controller to wake and pack", || {
        !scaling.sleeping() && scaling.active() == 1
    });
    scaling.stop();
}

#[test]
fn stopping_ends_the_thread_even_while_it_sleeps() {
    let workers = Fake::new(2);
    let scaling = Arc::new(Scaling::new("scaler-test-stop", policy(), 2));
    scaling.start(workers);
    wait_for("the controller to sleep", || scaling.sleeping());
    scaling.stop();
    assert!(scaling.stopped());
}

#[test]
fn live_scalers_are_listed_by_name() {
    let scaling = Arc::new(Scaling::new("scaler-test-listed", policy(), 3));
    let listed = oxiroute_worker_scaling::process_stats();
    let entry = listed
        .iter()
        .find(|(name, _)| name == "scaler-test-listed")
        .expect("listed");
    assert_eq!((entry.1.workers, entry.1.active), (3, 3));
    drop(scaling);
    assert!(
        oxiroute_worker_scaling::process_stats()
            .iter()
            .all(|(name, _)| name != "scaler-test-listed")
    );
}
