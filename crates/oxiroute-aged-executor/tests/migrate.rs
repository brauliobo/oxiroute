mod support;

use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use oxiroute_aged_executor::{Age, AgedExecutor, in_task, move_to};

/// A worker thread: a current-thread runtime that runs one executor.
struct Worker {
    executor: AgedExecutor,
    stop: tokio::sync::oneshot::Sender<()>,
    thread: thread::JoinHandle<()>,
}

fn worker(name: &'static str, measured: bool) -> Worker {
    let (executor, runner) = if measured {
        AgedExecutor::new_measured(Age::adaptive())
    } else {
        AgedExecutor::new(Age::adaptive())
    };
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let thread = thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                tokio::spawn(runner);
                let _ = stopped.await;
            });
        })
        .unwrap();
    Worker {
        executor,
        stop,
        thread,
    }
}

impl Worker {
    fn finish(self) {
        let _ = self.stop.send(());
        self.thread.join().unwrap();
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_task_continues_on_the_worker_it_moved_to_with_its_state_intact() {
    let (a, b) = (worker("migrate-a", false), worker("migrate-b", false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let target = b.executor.clone();
    let log = Arc::clone(&seen);
    a.executor.spawn(async move {
        let state = [1, 2, 3];
        log.lock()
            .unwrap()
            .push((thread::current().name().map(str::to_owned), in_task()));
        support::reschedule().await;
        assert!(move_to(target).await);
        support::reschedule().await;
        log.lock().unwrap().push((
            thread::current().name().map(str::to_owned),
            state.len() == 3,
        ));
    });
    wait_for("the task to finish", || seen.lock().unwrap().len() == 2);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0], (Some("migrate-a".into()), true));
    assert_eq!(seen[1], (Some("migrate-b".into()), true));
    assert_eq!((a.executor.stats().tasks, b.executor.stats().tasks), (0, 0));
    a.finish();
    b.finish();
}

#[test]
fn a_waker_a_task_left_behind_is_harmless_after_the_move() {
    let (a, b) = (
        worker("migrate-wake-a", false),
        worker("migrate-wake-b", false),
    );
    let (waker_tx, waker_rx) = std::sync::mpsc::channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let finished = Arc::new(Mutex::new(Vec::new()));
    let (target, finished_by) = (b.executor.clone(), Arc::clone(&finished));
    a.executor.spawn(async move {
        let waker = std::future::poll_fn(|cx| std::task::Poll::Ready(cx.waker().clone())).await;
        waker_tx.send(waker).unwrap();
        assert!(move_to(target).await);
        released.await.unwrap();
        finished_by
            .lock()
            .unwrap()
            .push(thread::current().name().map(str::to_owned));
    });
    let stale = waker_rx.recv().unwrap();
    thread::sleep(Duration::from_millis(100));
    stale.wake();
    thread::sleep(Duration::from_millis(50));
    assert!(
        finished.lock().unwrap().is_empty(),
        "a stale wake ran the task past its wait"
    );
    release.send(()).unwrap();
    wait_for("the task to finish on its new worker", || {
        !finished.lock().unwrap().is_empty()
    });
    assert_eq!(
        *finished.lock().unwrap(),
        [Some("migrate-wake-b".to_owned())]
    );
    a.finish();
    b.finish();
}

#[test]
fn outside_an_executor_task_moving_is_refused_instead_of_hanging() {
    let b = worker("migrate-outside", false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(!in_task());
        assert!(!move_to(b.executor.clone()).await);
    });
    b.finish();
}

#[test]
fn a_measured_executor_counts_the_time_its_tasks_run() {
    let w = worker("measured", true);
    assert_eq!(w.executor.busy_nanos(), 0);
    w.executor.spawn(async {
        thread::sleep(Duration::from_millis(30));
    });
    wait_for("the busy time to show", || {
        w.executor.busy_nanos() >= 30_000_000
    });
    w.finish();
}

#[test]
fn an_unmeasured_executor_does_not_read_the_clock_for_it() {
    let w = worker("unmeasured", false);
    w.executor.spawn(async {
        thread::sleep(Duration::from_millis(20));
    });
    wait_for("the task to finish", || w.executor.stats().tasks == 0);
    assert_eq!(w.executor.busy_nanos(), 0);
    w.finish();
}
