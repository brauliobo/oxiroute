//! The ready queue on a clock the test controls: the clock is what `begin_batch` and `pop` are given, so the order
//! tasks are served in and the waits they see do not depend on the speed or the load of the machine.

use std::{sync::Arc, time::Duration};

use super::*;

/// Tasks the runner polls between two reads of the clock.
const BATCH: usize = 32;
/// What one poll costs on the simulated clock.
const POLL: Duration = Duration::from_micros(1);

fn tasks_ready(shared: &Arc<Shared>, count: usize) -> Vec<Arc<TaskWaker>> {
    (0..count)
        .map(|id| {
            let task = Arc::new(TaskWaker::new(id, Arc::clone(shared)));
            task.queued.store(true, Ordering::Release);
            shared.push_ready(Arc::clone(&task));
            task
        })
        .collect()
}

fn stamp(shared: &Shared) -> Instant {
    shared.state().clock.expect("a stamped queue")
}

#[test]
fn the_newest_ready_task_is_served_until_the_oldest_has_waited_for_the_age() {
    let age = Duration::from_millis(5);
    let shared = Arc::new(Shared::new(Age::Fixed(age), false));
    let tasks = tasks_ready(&shared, 3);
    let queued = stamp(&shared);

    let newest = shared.pop(queued + age / 2).unwrap();
    assert_eq!(newest.id, tasks[2].id);
    let oldest = shared.pop(queued + age).unwrap();
    assert_eq!(oldest.id, tasks[0].id);
    assert_eq!(shared.stats().aged_polls, 1);
}

struct Flood {
    /// The waits (wake to poll) in the order the polls happened.
    waits: Vec<Duration>,
    /// When each task was first polled, from the start of the flood.
    first_poll: Vec<Option<Duration>>,
    stats: Stats,
}

/// Serves `tasks` always-ready tasks for `polls` polls: every task wakes itself right away, as one that yields does.
fn flood(age: Age, tasks: usize, polls: usize) -> Flood {
    let shared = Arc::new(Shared::new(age, false));
    let ready = tasks_ready(&shared, tasks);
    let start = stamp(&shared);
    let mut woken = vec![start; tasks];
    let mut first_poll = vec![None; tasks];
    let mut now = start;
    let mut waits = Vec::with_capacity(polls);
    while waits.len() < polls {
        let batch = now;
        shared.begin_batch(batch);
        for _ in 0..BATCH {
            let task = shared.pop(batch).expect("always-ready tasks");
            waits.push(now - woken[task.id]);
            first_poll[task.id].get_or_insert(now - start);
            now += POLL;
            woken[task.id] = batch;
            shared.push_ready(Arc::clone(&ready[task.id]));
        }
    }
    Flood {
        waits,
        first_poll,
        stats: shared.stats(),
    }
}

fn percentile(waits: &mut [Duration], per_mille: usize) -> Duration {
    waits.sort_unstable();
    waits[(waits.len() * per_mille / 1000).min(waits.len() - 1)]
}

const MULTIPLE: f64 = 4.0;

fn pass(tasks: usize) -> Duration {
    POLL * u32::try_from(tasks).unwrap()
}

/// By Little's law the mean wait of always-ready tasks is the time one pass over all of them takes, so an age that is a
/// multiple of the pass follows the ready queue and the speed of the machine.
#[test]
fn the_adaptive_age_is_a_multiple_of_one_pass_over_the_ready_tasks() {
    let stats = flood(Age::Adaptive(MULTIPLE), 1024, 100_000).stats;

    let expected = pass(1024).mul_f64(MULTIPLE);
    assert!(
        stats.age.abs_diff(expected) < expected / 100,
        "age {:?}, {MULTIPLE} passes {expected:?}",
        stats.age
    );
}

#[test]
fn a_short_ready_queue_gets_the_smallest_age() {
    let stats = flood(Age::Adaptive(MULTIPLE), 4, 10_000).stats;

    assert_eq!(stats.age, Age::MIN);
}

/// Few ready tasks fit the caches, so newest-first has nothing to gain and the age stays at one pass.
#[test]
fn a_queue_below_the_ramp_has_an_age_of_about_one_pass() {
    let stats = flood(Age::Adaptive(MULTIPLE), 32, 10_000).stats;

    assert!(
        stats.age <= pass(32) * 2 + Age::MIN * 2,
        "age {:?}",
        stats.age
    );
}

/// No task waits longer than the age plus the time to serve the tasks that were already older than the age when it ran
/// out, which are all of them at worst.
#[test]
fn the_p999_wait_stays_within_a_few_passes_under_a_flood_of_newer_connections() {
    const TASKS: usize = 2048;
    let Flood {
        mut waits, stats, ..
    } = flood(Age::Adaptive(MULTIPLE), TASKS, 400_000);

    assert!(
        stats.aged_polls > 0,
        "the oldest task was never served first"
    );
    let bound = pass(TASKS).mul_f64(3.0 * MULTIPLE);
    let p999 = percentile(&mut waits, 999);
    assert!(p999 <= bound, "p99.9 {p999:?}, pass {:?}", pass(TASKS));
}

const AGE: Duration = Duration::from_millis(25);

/// Newest-first alone has no bound on a wait: the same few tasks are served for as long as the flood lasts.
#[test]
fn without_an_age_the_oldest_task_starves_under_a_flood_of_newer_ones() {
    let run = flood(Age::Fixed(Duration::from_hours(1)), 64, 400_000);

    assert_eq!(run.first_poll[0], None);
    assert_eq!(run.stats.aged_polls, 0);
}

#[test]
fn with_an_age_the_oldest_task_runs_within_the_age_under_a_flood_of_newer_ones() {
    let run = flood(Age::Fixed(AGE), 64, 400_000);

    let wait = run.first_poll[0].expect("the oldest task ran during the flood");
    assert!(wait < AGE * 4, "{wait:?}");
}

/// 2048 connections that are always ready: the tail of the wait stays within a small multiple of the age. That is the
/// bound by construction; newest-first alone has none (the first test shows a task that never runs).
#[test]
fn the_p999_wait_stays_within_twice_the_age_under_a_flood_of_newer_connections() {
    let Flood {
        mut waits, stats, ..
    } = flood(Age::Fixed(AGE), 2048, 1_000_000);

    let p999 = percentile(&mut waits, 999);
    assert!(p999 <= AGE * 2, "p99.9 {p999:?}");
    assert!(stats.aged_polls > 0);
}
