mod support;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use oxiroute_aged_executor::{Age, AgedExecutor};
use tokio::{sync::oneshot, time::timeout};

/// A task that was ready before a flood of newer ones must not starve: pure newest-first never runs it. The wait is
/// for the event; the bound on how long a task may wait is checked on a simulated clock in `queue::tests`.
#[tokio::test(flavor = "current_thread")]
async fn the_oldest_task_runs_under_a_flood_of_newer_ones() {
    let (executor, runner) = AgedExecutor::new(Age::Adaptive(4.0));
    let stop = Arc::new(AtomicBool::new(false));
    let (ran, oldest) = oneshot::channel();
    executor.spawn(async move {
        support::reschedule().await;
        let _ = ran.send(());
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
    let ran = timeout(std::time::Duration::from_mins(1), oldest).await;
    stop.store(true, Ordering::Relaxed);
    runner.abort();
    assert!(ran.is_ok(), "the oldest task never ran");
    assert!(executor.stats().aged_polls > 0);
}
