//! The controller on a fake clock: every test feeds it the busy share of each worker once per tick.

use std::time::Duration;

use oxiroute_worker_scaling::{Controller, Policy};

const TICK: Duration = Duration::from_millis(20);

fn policy(min_active: usize) -> Policy {
    Policy {
        min_active,
        target_busy_permille: 500,
        release_delay: Duration::from_secs(5),
    }
}

/// A controller and the clock it is stepped by.
struct Rig {
    controller: Controller,
    now: Duration,
}

impl Rig {
    fn new(policy: Policy, workers: usize) -> Self {
        Self {
            controller: Controller::new(policy, workers),
            now: Duration::ZERO,
        }
    }

    /// Steps for `span` with the same busy share on every worker; returns the active count after the last tick.
    fn run(&mut self, span: Duration, busy: &[u32]) -> usize {
        let end = self.now + span;
        while self.now < end {
            self.now += TICK;
            self.controller.step(self.now, busy);
        }
        self.controller.active()
    }
}

fn spread(total_permille: u32, workers: usize) -> Vec<u32> {
    let share = total_permille / u32::try_from(workers).unwrap();
    vec![share; workers]
}

#[test]
fn every_worker_is_active_at_the_start() {
    assert_eq!(Controller::new(policy(1), 8).active(), 8);
}

#[test]
fn a_light_load_is_packed_onto_the_minimum_once_the_delay_has_passed() {
    let mut rig = Rig::new(policy(1), 8);
    let light = spread(160, 8);
    assert_eq!(
        rig.run(Duration::from_millis(4_900), &light),
        8,
        "released before the delay"
    );
    assert_eq!(rig.run(Duration::from_millis(300), &light), 1);
}

#[test]
fn the_release_goes_straight_to_the_workers_the_load_fits() {
    let mut rig = Rig::new(policy(1), 8);
    // 700 permille of work: at 60% of the 500 target a worker takes 300, so three workers hold it.
    let load = spread(700, 8);
    assert_eq!(rig.run(Duration::from_secs(6), &load), 3);
}

#[test]
fn the_minimum_is_never_released() {
    let mut rig = Rig::new(policy(3), 8);
    assert_eq!(rig.run(Duration::from_secs(30), &spread(0, 8)), 3);
}

#[test]
fn a_spike_inside_the_delay_restarts_it() {
    let mut rig = Rig::new(policy(1), 4);
    let light = spread(100, 4);
    assert_eq!(rig.run(Duration::from_secs(4), &light), 4);
    // Busy enough that three workers are needed, for one tick of the window.
    assert_eq!(rig.run(TICK, &spread(1_200, 4)), 4);
    assert_eq!(
        rig.run(Duration::from_secs(4), &light),
        4,
        "the delay restarted at the spike"
    );
    assert_eq!(rig.run(Duration::from_secs(2), &light), 1);
}

#[test]
fn overload_activates_workers_within_a_few_ticks() {
    let mut rig = Rig::new(policy(1), 8);
    rig.run(Duration::from_secs(6), &spread(100, 8));
    assert_eq!(rig.controller.active(), 1);
    // The one active worker is saturated: the demand it shows is 1000 permille, two workers' worth at the target.
    let mut ticks = 0;
    while rig.controller.active() < 2 {
        rig.now += TICK;
        rig.controller.step(rig.now, &[1_000, 0, 0, 0, 0, 0, 0, 0]);
        ticks += 1;
        assert!(ticks <= 5, "no worker was activated in {ticks} ticks");
    }
}

#[test]
fn saturated_workers_roughly_double_the_active_set_until_all_are_active() {
    let mut rig = Rig::new(policy(1), 8);
    rig.run(Duration::from_secs(6), &spread(100, 8));
    let mut busy = vec![0; 8];
    for _ in 0..40 {
        let active = rig.controller.active();
        busy.fill(0);
        busy[..active].fill(1_000);
        rig.now += TICK;
        rig.controller.step(rig.now, &busy);
    }
    assert_eq!(rig.controller.active(), 8);
}

#[test]
fn a_load_between_the_release_and_the_target_bands_changes_nothing() {
    let mut rig = Rig::new(policy(1), 4);
    // 500 permille of work: two workers at 250 each, the fewest that stay under 60% of the target.
    assert_eq!(rig.run(Duration::from_secs(6), &spread(500, 4)), 2);
    // Two workers would be at 225 (above the release band for one) and 450 (under the target for two): no change.
    assert_eq!(rig.run(Duration::from_secs(20), &spread(450, 4)), 2);
    assert_eq!(rig.run(Duration::from_secs(20), &spread(900, 4)), 2);
}

#[test]
fn work_left_on_released_workers_counts_as_load() {
    let mut rig = Rig::new(policy(1), 4);
    rig.run(Duration::from_secs(6), &spread(40, 4));
    assert_eq!(rig.controller.active(), 1);
    // Connections that cannot move keep two released workers at 450 permille each.
    rig.run(Duration::from_millis(200), &[300, 450, 450, 0]);
    assert!(
        rig.controller.active() >= 2,
        "the released workers' work was ignored"
    );
}

#[test]
fn the_active_count_never_leaves_the_bounds() {
    let mut rig = Rig::new(policy(2), 4);
    for busy in [0, 1_000, 0, 1_000, 500, 0] {
        for _ in 0..400 {
            rig.now += TICK;
            let active = rig.controller.step(rig.now, &spread(busy * 4, 4));
            assert!((2..=4).contains(&active));
        }
    }
}
