mod support;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_aged_executor::AgedExecutor;

const AGE: Duration = Duration::from_millis(25);

/// `flood` tasks that are ready all the time, like the keep-alive connections of a saturated worker: each notes how
/// long it waited between waking itself and being polled again.
fn spawn_flood(
    executor: &AgedExecutor,
    flood: usize,
    stop: &Arc<AtomicBool>,
) -> Arc<Mutex<Vec<Duration>>> {
    let waits = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..flood {
        let (stop, waits) = (Arc::clone(stop), Arc::clone(&waits));
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
    waits
}

fn percentile(waits: &mut [Duration], per_mille: usize) -> Duration {
    waits.sort_unstable();
    waits[(waits.len() * per_mille / 1000).min(waits.len() - 1)]
}

/// A task that was ready before the flood started: when did it run for the second time, if during the flood?
async fn victim_wait(max_age: Duration, window: Duration) -> Option<Duration> {
    let (executor, runner) = AgedExecutor::new(max_age);
    let stop = Arc::new(AtomicBool::new(false));
    let ran_at = Arc::new(Mutex::new(None));
    let started = Instant::now();
    let seen = Arc::clone(&ran_at);
    executor.spawn(async move {
        support::reschedule().await;
        *seen.lock().unwrap() = Some(started.elapsed());
    });
    spawn_flood(&executor, 64, &stop);
    let runner = tokio::spawn(runner);
    tokio::time::sleep(window).await;
    let during_flood = *ran_at.lock().unwrap();
    stop.store(true, Ordering::Relaxed);
    while ran_at.lock().unwrap().is_none() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    runner.abort();
    during_flood
}

#[tokio::test(flavor = "current_thread")]
async fn without_an_age_the_oldest_task_starves_under_a_flood_of_newer_ones() {
    let wait = victim_wait(Duration::from_hours(1), Duration::from_millis(400)).await;
    assert_eq!(wait, None);
}

#[tokio::test(flavor = "current_thread")]
async fn with_an_age_the_oldest_task_runs_within_the_age_under_a_flood_of_newer_ones() {
    let wait = victim_wait(AGE, Duration::from_millis(400))
        .await
        .expect("the oldest task ran during the flood");
    assert!(wait < AGE * 4, "{wait:?}");
}

/// 2048 connections that are always ready: the tail of the wait stays within a small multiple of the age. That is the
/// bound by construction; newest-first alone has none (the first test shows a task that never runs).
#[tokio::test(flavor = "current_thread")]
async fn the_p999_wait_stays_within_twice_the_age_under_a_flood_of_newer_connections() {
    let (executor, runner) = AgedExecutor::new(AGE);
    let stop = Arc::new(AtomicBool::new(false));
    let waits = spawn_flood(&executor, 2048, &stop);
    let runner = tokio::spawn(runner);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    stop.store(true, Ordering::Relaxed);
    while executor.stats().tasks > 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    runner.abort();
    let mut waits = waits.lock().unwrap().clone();
    assert!(waits.len() > 10_000, "{} polls", waits.len());
    let p999 = percentile(&mut waits, 999);
    assert!(
        p999 <= AGE * 2 + Duration::from_millis(100),
        "p99.9 {p999:?}"
    );
    assert!(executor.stats().aged_polls > 0);
}
