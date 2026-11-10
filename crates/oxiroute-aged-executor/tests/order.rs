mod support;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use oxiroute_aged_executor::AgedExecutor;
use tokio::sync::mpsc;

const HOUR: Duration = Duration::from_hours(1);

/// Spawns one task per name before the runner starts and returns the order in which they ran.
async fn order_of(max_age: Duration, names: &[&'static str]) -> Vec<&'static str> {
    let (executor, runner) = AgedExecutor::new(max_age);
    let ran = Arc::new(Mutex::new(Vec::new()));
    let (done, mut finished) = mpsc::unbounded_channel();
    for &name in names {
        let (ran, done) = (Arc::clone(&ran), done.clone());
        executor.spawn(async move {
            ran.lock().unwrap().push(name);
            done.send(()).unwrap();
        });
    }
    let runner = tokio::spawn(runner);
    for _ in names {
        finished.recv().await.unwrap();
    }
    runner.abort();
    ran.lock().unwrap().clone()
}

#[tokio::test(flavor = "current_thread")]
async fn the_newest_ready_task_runs_first_while_nothing_has_aged() {
    assert_eq!(order_of(HOUR, &["a", "b", "c"]).await, ["c", "b", "a"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_zero_age_is_first_in_first_out() {
    assert_eq!(
        order_of(Duration::ZERO, &["a", "b", "c"]).await,
        ["a", "b", "c"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_task_woken_from_another_thread_runs() {
    let (executor, runner) = AgedExecutor::new(HOUR);
    let runner = tokio::spawn(runner);
    let (wake, woken) = tokio::sync::oneshot::channel();
    let (done, finished) = tokio::sync::oneshot::channel();
    executor.spawn(async move {
        woken.await.unwrap();
        done.send(()).unwrap();
    });
    std::thread::spawn(move || wake.send(()).unwrap());
    tokio::time::timeout(Duration::from_secs(10), finished)
        .await
        .expect("woken task ran")
        .unwrap();
    runner.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn a_panicking_task_does_not_stop_the_others() {
    let (executor, runner) = AgedExecutor::new(HOUR);
    let runner = tokio::spawn(runner);
    let (done, mut finished) = mpsc::unbounded_channel();
    executor.spawn(async { panic!("expected by the test") });
    for _ in 0..3 {
        let done = done.clone();
        executor.spawn(async move {
            support::reschedule().await;
            done.send(()).unwrap();
        });
    }
    for _ in 0..3 {
        finished.recv().await.unwrap();
    }
    assert!(!runner.is_finished());
    runner.abort();
}

struct CountDrops(Arc<AtomicUsize>);

impl Drop for CountDrops {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_the_runner_drops_every_task_and_refuses_new_ones() {
    let (executor, runner) = AgedExecutor::new(HOUR);
    let drops = Arc::new(AtomicUsize::new(0));
    let runner = tokio::spawn(runner);
    for _ in 0..3 {
        let guard = CountDrops(Arc::clone(&drops));
        executor.spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
    }
    while executor.stats().tasks < 3 {
        tokio::task::yield_now().await;
    }
    runner.abort();
    assert!(runner.await.unwrap_err().is_cancelled());
    assert_eq!(drops.load(Ordering::SeqCst), 3);

    let guard = CountDrops(Arc::clone(&drops));
    executor.spawn(async move {
        let _guard = guard;
    });
    assert_eq!(drops.load(Ordering::SeqCst), 4);
}

#[tokio::test(flavor = "current_thread")]
async fn statistics_count_tasks_polls_and_aged_polls() {
    let (executor, runner) = AgedExecutor::new(Duration::ZERO);
    let (done, mut finished) = mpsc::unbounded_channel();
    for _ in 0..4 {
        let done = done.clone();
        executor.spawn(async move { done.send(()).unwrap() });
    }
    let runner = tokio::spawn(runner);
    for _ in 0..4 {
        finished.recv().await.unwrap();
    }
    let stats = executor.stats();
    assert!(stats.polls >= 4 && stats.aged_polls >= 4, "{stats:?}");
    assert_eq!(stats.tasks, 0);
    assert!(AgedExecutor::process_stats().polls >= stats.polls);
    runner.abort();
}
