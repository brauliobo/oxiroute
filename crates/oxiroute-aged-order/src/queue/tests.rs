//! The ready queue on a clock the test controls: the clock is what `begin_batch` and `pop` are given, so the order
//! items are served in and the waits they see do not depend on the speed or the load of the machine.

use std::time::Duration;

use super::*;

const BATCH: usize = 32;
const POLL: Duration = Duration::from_micros(1);

fn ready(queue: &mut AgedQueue<usize>, count: usize) {
    (0..count).for_each(|id| queue.push(id));
}

fn stamp(queue: &AgedQueue<usize>) -> Instant {
    queue.clock.expect("a stamped queue")
}

#[test]
fn the_newest_ready_item_is_served_until_the_oldest_has_waited_for_the_age() {
    let age = Duration::from_millis(5);
    let mut queue = AgedQueue::new(Age::Fixed(age));
    ready(&mut queue, 3);
    let queued = stamp(&queue);

    assert_eq!(queue.pop(queued + age / 2), Some(2));
    assert_eq!(queue.pop(queued + age), Some(0));
    assert_eq!(queue.stats(3).aged_polls, 1);
}

#[test]
fn an_empty_queue_serves_nothing() {
    let mut queue = AgedQueue::<usize>::new(Age::Fixed(Duration::ZERO));

    assert_eq!(queue.pop(Instant::now()), None);
}

struct Flood {
    waits: Vec<Duration>,
    first_poll: Vec<Option<Duration>>,
    stats: Stats,
}

fn flood(age: Age, items: usize, polls: usize) -> Flood {
    let mut queue = AgedQueue::new(age);
    ready(&mut queue, items);
    let start = stamp(&queue);
    let mut woken = vec![start; items];
    let mut first_poll = vec![None; items];
    let mut now = start;
    let mut waits = Vec::with_capacity(polls);
    while waits.len() < polls {
        let batch = now;
        queue.begin_batch(batch, 0);
        for _ in 0..BATCH {
            let id = queue.pop(batch).expect("always-ready items");
            waits.push(now - woken[id]);
            first_poll[id].get_or_insert(now - start);
            now += POLL;
            woken[id] = batch;
            queue.push(id);
        }
    }
    Flood {
        waits,
        first_poll,
        stats: queue.stats(items),
    }
}

fn percentile(waits: &mut [Duration], per_mille: usize) -> Duration {
    waits.sort_unstable();
    waits[(waits.len() * per_mille / 1000).min(waits.len() - 1)]
}

const MULTIPLE: f64 = 4.0;

fn pass(items: usize) -> Duration {
    POLL * u32::try_from(items).unwrap()
}

#[test]
fn the_adaptive_age_is_a_multiple_of_one_pass_over_the_ready_items() {
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

#[test]
fn a_queue_below_the_ramp_has_an_age_of_about_one_pass() {
    let stats = flood(Age::Adaptive(MULTIPLE), 32, 10_000).stats;

    assert!(
        stats.age <= pass(32) * 2 + Age::MIN * 2,
        "age {:?}",
        stats.age
    );
}

#[test]
fn the_p999_wait_stays_within_a_few_passes_under_a_flood_of_newer_items() {
    const ITEMS: usize = 2048;
    let Flood {
        mut waits, stats, ..
    } = flood(Age::Adaptive(MULTIPLE), ITEMS, 400_000);

    assert!(
        stats.aged_polls > 0,
        "the oldest item was never served first"
    );
    let bound = pass(ITEMS).mul_f64(3.0 * MULTIPLE);
    let p999 = percentile(&mut waits, 999);
    assert!(p999 <= bound, "p99.9 {p999:?}, pass {:?}", pass(ITEMS));
}

const AGE: Duration = Duration::from_millis(25);

#[test]
fn without_an_age_the_oldest_item_starves_under_a_flood_of_newer_ones() {
    let run = flood(Age::Fixed(Duration::from_hours(1)), 64, 400_000);

    assert_eq!(run.first_poll[0], None);
    assert_eq!(run.stats.aged_polls, 0);
}

#[test]
fn with_an_age_the_oldest_item_runs_within_the_age_under_a_flood_of_newer_ones() {
    let run = flood(Age::Fixed(AGE), 64, 400_000);

    let wait = run.first_poll[0].expect("the oldest item ran during the flood");
    assert!(wait < AGE * 4, "{wait:?}");
}

#[test]
fn the_p999_wait_stays_within_twice_the_age_under_a_flood_of_newer_items() {
    let Flood {
        mut waits, stats, ..
    } = flood(Age::Fixed(AGE), 2048, 1_000_000);

    let p999 = percentile(&mut waits, 999);
    assert!(p999 <= AGE * 2, "p99.9 {p999:?}");
    assert!(stats.aged_polls > 0);
}
