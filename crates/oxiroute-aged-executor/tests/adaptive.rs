mod support;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_aged_executor::{Age, AgedExecutor};

const MULTIPLE: f64 = 4.0;

struct Flood {
    waits: Vec<Duration>,
    age: Duration,
    /// How long one pass over all the tasks took: their number over the polls per second.
    pass: Duration,
}

/// `flood` always-ready tasks, run for `window`; the waits (wake to poll) they saw. The mean wait is not a pass:
/// newest-first polls a few tasks very often.
#[allow(clippy::cast_precision_loss)]
async fn flood(age: Age, flood: usize, window: Duration) -> Flood {
    let (executor, runner) = AgedExecutor::new(age);
    let stop = Arc::new(AtomicBool::new(false));
    let waits = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..flood {
        let (stop, waits) = (Arc::clone(&stop), Arc::clone(&waits));
        executor.spawn(async move {
            let mut local = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let woke = Instant::now();
                support::reschedule().await;
                local.push(woke.elapsed());
            }
            waits.lock().unwrap().extend(local);
        });
    }
    let runner = tokio::spawn(runner);
    tokio::time::sleep(window).await;
    let stats = executor.stats();
    stop.store(true, Ordering::Relaxed);
    while executor.stats().tasks > 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    runner.abort();
    let waits = waits.lock().unwrap().clone();
    let pass = window.mul_f64(flood as f64 / stats.polls as f64);
    Flood {
        waits,
        age: stats.age,
        pass,
    }
}

fn percentile(waits: &mut [Duration], per_mille: usize) -> Duration {
    waits.sort_unstable();
    waits[(waits.len() * per_mille / 1000).min(waits.len() - 1)]
}

/// By Little's law the mean wait of always-ready tasks is the time one pass over all of them takes, so an age that
/// is a multiple of the pass follows the ready queue and the speed of the machine.
#[tokio::test(flavor = "current_thread")]
async fn the_adaptive_age_is_a_multiple_of_one_pass_over_the_ready_tasks() {
    let run = flood(Age::Adaptive(MULTIPLE), 1024, Duration::from_millis(800)).await;
    let expected = run.pass.mul_f64(MULTIPLE);
    let age = run.age;
    assert!(
        age > expected / 4 && age < expected * 4,
        "age {age:?}, {MULTIPLE} passes {expected:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_short_ready_queue_gets_the_smallest_age() {
    let age = flood(Age::Adaptive(MULTIPLE), 4, Duration::from_millis(300))
        .await
        .age;
    assert!(age <= Duration::from_millis(2), "{age:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn the_p999_wait_stays_within_a_few_passes_under_a_flood_of_newer_connections() {
    let mut run = flood(Age::Adaptive(MULTIPLE), 2048, Duration::from_millis(1500)).await;
    assert!(run.waits.len() > 10_000, "{} polls", run.waits.len());
    let pass = run.pass;
    let p999 = percentile(&mut run.waits, 999);
    // The age, plus the tasks that were already overdue when it ran out, plus a worker thread that the loaded host
    // takes off its core now and then.
    let bound = pass.mul_f64(3.0 * MULTIPLE) + Duration::from_millis(50);
    assert!(p999 <= bound, "p99.9 {p999:?}, pass {pass:?}");
}

/// A task that was ready before a flood of newer ones must not starve: pure newest-first never runs it.
#[tokio::test(flavor = "current_thread")]
async fn the_oldest_task_runs_under_a_flood_of_newer_ones() {
    let (executor, runner) = AgedExecutor::new(Age::Adaptive(MULTIPLE));
    let stop = Arc::new(AtomicBool::new(false));
    let ran_after = Arc::new(Mutex::new(None));
    let (started, seen) = (Instant::now(), Arc::clone(&ran_after));
    executor.spawn(async move {
        support::reschedule().await;
        *seen.lock().unwrap() = Some(started.elapsed());
    });
    for _ in 0..256 {
        let stop = Arc::clone(&stop);
        executor.spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                support::reschedule().await;
            }
        });
    }
    let runner = tokio::spawn(runner);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let wait = *ran_after.lock().unwrap();
    stop.store(true, Ordering::Relaxed);
    runner.abort();
    assert!(
        wait.is_some_and(|wait| wait < Duration::from_millis(200)),
        "{wait:?}"
    );
}
