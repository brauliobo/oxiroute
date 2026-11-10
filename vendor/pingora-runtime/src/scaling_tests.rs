//! Worker scaling on a real non-stealing runtime: placement on the active workers, relocation off released ones and
//! activation under load.

use super::*;
use std::collections::HashSet;
use std::sync::Mutex;
use std::thread::ThreadId;
use tokio::sync::Notify;

const WORKERS: usize = 4;

fn runtime(min_active: usize) -> NoStealRuntime {
    NoStealRuntime::new(
        WORKERS,
        "scaling-test",
        BlockingPoolOpts::default(),
        RuntimeOpts {
            connection_age: Some(Age::adaptive()),
            worker_scaling: Some(ScalingPolicy {
                min_active,
                target_busy_permille: 500,
                release_delay: Duration::from_millis(150),
            }),
            ..RuntimeOpts::default()
        },
    )
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Starts `count` connections from a worker thread; each records its thread, waits to be released, records its thread
/// again (after asking to move when `relocate` is set) and ends.
struct Fleet {
    started: Arc<Mutex<Vec<ThreadId>>>,
    ended: Arc<Mutex<Vec<ThreadId>>>,
    release: Arc<Notify>,
}

impl Fleet {
    fn start(runtime: &NoStealRuntime, count: usize, relocate: bool) -> Self {
        let fleet = Self {
            started: Arc::default(),
            ended: Arc::default(),
            release: Arc::new(Notify::new()),
        };
        let (started, ended, release) = (
            fleet.started.clone(),
            fleet.ended.clone(),
            fleet.release.clone(),
        );
        runtime.get_runtime_at(0).spawn(async move {
            for _ in 0..count {
                let (started, ended, release) = (started.clone(), ended.clone(), release.clone());
                spawn_connection(async move {
                    started.lock().unwrap().push(std::thread::current().id());
                    release.notified().await;
                    if relocate && wants_relocation() {
                        relocate_connection().await;
                    }
                    ended.lock().unwrap().push(std::thread::current().id());
                });
            }
        });
        fleet
    }

    fn wait_started(&self, count: usize) {
        wait_for("the connections to start", || {
            self.started.lock().unwrap().len() == count
        });
    }

    fn threads(list: &Mutex<Vec<ThreadId>>) -> HashSet<ThreadId> {
        list.lock().unwrap().iter().copied().collect()
    }
}

fn stats(runtime: &NoStealRuntime) -> ScalingStats {
    runtime.scaling_stats().expect("scaling runs")
}

#[test]
fn new_connections_spread_over_all_workers_until_the_load_proves_light() {
    let runtime = runtime(1);
    let fleet = Fleet::start(&runtime, 8, false);
    fleet.wait_started(8);
    assert_eq!(Fleet::threads(&fleet.started).len(), WORKERS);
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn a_light_load_is_packed_and_later_connections_land_on_the_active_worker() {
    let runtime = runtime(1);
    let first = Fleet::start(&runtime, 8, false);
    first.wait_started(8);
    wait_for("the light load to be packed", || {
        stats(&runtime).active == 1
    });
    let later = Fleet::start(&runtime, 16, false);
    later.wait_started(16);
    assert_eq!(Fleet::threads(&later.started).len(), 1);
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn idle_connections_move_off_released_workers_to_the_active_one() {
    let runtime = runtime(1);
    let fleet = Fleet::start(&runtime, 8, true);
    fleet.wait_started(8);
    assert_eq!(Fleet::threads(&fleet.started).len(), WORKERS);
    wait_for("the light load to be packed", || {
        stats(&runtime).active == 1
    });
    fleet.release.notify_waiters();
    wait_for("the connections to end", || {
        fleet.ended.lock().unwrap().len() == 8
    });
    assert_eq!(Fleet::threads(&fleet.ended).len(), 1);
    assert_eq!(
        stats(&runtime).relocations,
        6,
        "two of the eight were on the active worker already"
    );
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn a_connection_that_has_not_asked_stays_where_it_is() {
    let runtime = runtime(1);
    let fleet = Fleet::start(&runtime, 8, false);
    fleet.wait_started(8);
    wait_for("the light load to be packed", || {
        stats(&runtime).active == 1
    });
    fleet.release.notify_waiters();
    wait_for("the connections to end", || {
        fleet.ended.lock().unwrap().len() == 8
    });
    assert_eq!(Fleet::threads(&fleet.ended), Fleet::threads(&fleet.started));
    assert_eq!(stats(&runtime).relocations, 0);
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn a_saturated_worker_activates_more_workers() {
    let runtime = runtime(1);
    let first = Fleet::start(&runtime, 1, false);
    first.wait_started(1);
    wait_for("the light load to be packed", || {
        stats(&runtime).active == 1
    });
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spin = stop.clone();
    runtime.get_runtime_at(0).spawn(async move {
        // Two always-ready connections that block the active worker's thread most of the time.
        for _ in 0..2 {
            let spin = spin.clone();
            spawn_connection(async move {
                while !spin.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                    tokio::task::yield_now().await;
                }
            });
        }
    });
    wait_for("more workers to be activated", || {
        stats(&runtime).active >= 2
    });
    stop.store(true, Ordering::Relaxed);
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn without_scaling_options_nothing_is_controlled_and_the_old_placement_holds() {
    let runtime = NoStealRuntime::new(
        WORKERS,
        "scaling-test-off",
        BlockingPoolOpts::default(),
        RuntimeOpts {
            connection_age: Some(Age::adaptive()),
            ..RuntimeOpts::default()
        },
    );
    let fleet = Fleet::start(&runtime, 8, true);
    fleet.wait_started(8);
    assert!(runtime.scaling_stats().is_none());
    assert_eq!(Fleet::threads(&fleet.started).len(), WORKERS);
    std::thread::sleep(Duration::from_millis(100));
    fleet.release.notify_waiters();
    wait_for("the connections to end", || {
        fleet.ended.lock().unwrap().len() == 8
    });
    assert_eq!(
        Fleet::threads(&fleet.ended),
        Fleet::threads(&fleet.started),
        "relocation is off"
    );
    runtime.shutdown_timeout(Duration::from_secs(1));
}
