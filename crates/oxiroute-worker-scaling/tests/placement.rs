//! Where connections go: new ones to the least loaded active worker, idle ones off released or crowded workers.

use oxiroute_worker_scaling::{Policy, Scaling};

use std::time::Duration;

fn scaling(workers: usize) -> Scaling {
    Scaling::new(
        "placement-test",
        Policy {
            min_active: 1,
            target_busy_permille: 500,
            release_delay: Duration::from_secs(5),
        },
        workers,
    )
}

#[test]
fn a_new_connection_goes_to_the_least_loaded_active_worker() {
    let scaling = scaling(4);
    scaling.set_active(3);
    let loads = [5, 2, 9, 0];
    assert_eq!(
        scaling.place(0, |worker| loads[worker]),
        1,
        "worker 3 is released: it takes nothing new"
    );
}

#[test]
fn a_tie_goes_to_the_calling_worker() {
    let scaling = scaling(4);
    assert_eq!(scaling.place(2, |_| 7), 2);
    scaling.set_active(2);
    assert_eq!(
        scaling.place(3, |_| 7),
        0,
        "a released caller falls back to the first active worker"
    );
    assert_eq!(scaling.place(1, |_| 7), 1);
}

#[test]
fn a_connection_on_a_released_worker_moves_to_the_least_loaded_active_one() {
    let scaling = scaling(4);
    scaling.set_active(2);
    let loads = [4, 3, 1, 8];
    assert!(scaling.wants_relocation(3, loads[3]));
    assert_eq!(scaling.relocation(3, |worker| loads[worker]), Some(1));
}

#[test]
fn a_connection_on_a_balanced_worker_stays() {
    let scaling = scaling(4);
    scaling.set_rebalance_above(40);
    let loads = [32, 31, 33, 32];
    assert!(!scaling.wants_relocation(0, loads[0]));
    assert_eq!(scaling.relocation(0, |worker| loads[worker]), None);
}

#[test]
fn a_crowded_worker_sheds_to_a_clearly_lighter_one_only() {
    let scaling = scaling(4);
    scaling.set_rebalance_above(40);
    let loads = [90, 20, 30, 40];
    assert!(scaling.wants_relocation(0, loads[0]));
    assert_eq!(scaling.relocation(0, |worker| loads[worker]), Some(1));
    // The threshold lags the loads: a worker above it with nothing clearly lighter to go to stays.
    let loads = [41, 40, 40, 41];
    assert!(scaling.wants_relocation(0, loads[0]));
    assert_eq!(scaling.relocation(0, |worker| loads[worker]), None);
}
