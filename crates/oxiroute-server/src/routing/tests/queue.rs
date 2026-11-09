use super::*;

pub(super) async fn wait_for_queued(pool: &EndpointPool, expected: u64) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.health_snapshot().queued != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued request count");
}

#[tokio::test]
async fn bounded_capacity_queue_releases_and_times_out_exactly_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "queued".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_millis(30)),
        )
        .expect("queued pool"),
    );
    let held = pool.select().expect("initial capacity");
    let waiting_pool = Arc::clone(&pool);
    let waiter = tokio::spawn(async move { waiting_pool.select_wait().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.health_snapshot().queued != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("waiter entered queue");
    drop(held);
    let acquired = waiter
        .await
        .expect("waiter task")
        .expect("released capacity");
    assert_eq!(acquired.server_name(), "only");
    drop(acquired);
    let held = pool.select().expect("capacity after release");
    assert!(pool.select_wait().await.is_none());
    drop(held);

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 1);
    assert_eq!(snapshot.queue_cancellations, 0);
    assert_eq!(snapshot.endpoints[0].active_connections, 0);
}

#[tokio::test]
async fn queue_disabled_selection_ignores_a_transient_fifo_registration() {
    let pool = RoundRobinPool::new_named_servers(
        "immediate".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("immediate pool");
    let _concurrent_front =
        QueueWaitGuard::new(Arc::clone(&pool.queue), "fifo", QueueTag::default());

    let acquired = pool
        .select_wait()
        .await
        .expect("queue-disabled selection must attempt available capacity");

    drop(acquired);
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 0);
}

#[tokio::test]
async fn fifo_queue_sends_the_oldest_waiter_to_whichever_endpoint_frees_first() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "fifo".into(),
            [
                runtime_server("primary", 3000, Some(1)),
                runtime_server("secondary", 3001, Some(1)),
            ],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("FIFO pool"),
    );
    let primary = pool.select().expect("primary capacity");
    let secondary = pool.select().expect("secondary capacity");
    let old_pool = Arc::clone(&pool);
    let old = tokio::spawn(async move { old_pool.select_wait().await });
    wait_for_queued(&pool, 1).await;
    let new_pool = Arc::clone(&pool);
    let new = tokio::spawn(async move { new_pool.select_wait().await });
    wait_for_queued(&pool, 2).await;

    drop(secondary);
    let old = old
        .await
        .expect("old waiter task")
        .expect("old waiter lease");
    assert_eq!(old.server_name(), "secondary");
    wait_for_queued(&pool, 1).await;
    drop(primary);
    let new = new
        .await
        .expect("new waiter task")
        .expect("new waiter lease");
    assert_eq!(new.server_name(), "primary");
    drop((old, new));

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 0);
    assert_eq!(snapshot.queue_cancellations, 0);
}

#[tokio::test]
async fn cancelling_the_fifo_head_advances_the_next_waiter_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "fifo-cancel".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("FIFO cancellation pool"),
    );
    let held = pool.select().expect("held capacity");
    let old_pool = Arc::clone(&pool);
    let old = tokio::spawn(async move { old_pool.select_wait().await });
    wait_for_queued(&pool, 1).await;
    let new_pool = Arc::clone(&pool);
    let new = tokio::spawn(async move { new_pool.select_wait().await });
    wait_for_queued(&pool, 2).await;

    old.abort();
    assert!(old.await.expect_err("old waiter cancelled").is_cancelled());
    wait_for_queued(&pool, 1).await;
    drop(held);
    let acquired = new
        .await
        .expect("new waiter task")
        .expect("new waiter lease");
    drop(acquired);

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 0);
    assert_eq!(snapshot.queue_cancellations, 1);
}

#[tokio::test]
async fn timing_out_the_fifo_head_exposes_free_capacity_to_the_next_waiter() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "fifo-timeout".into(),
            [
                runtime_server("primary", 3000, Some(1)),
                runtime_server("secondary", 3001, Some(1)),
            ],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_millis(100)),
        )
        .expect("FIFO timeout pool"),
    );
    let primary = pool.select().expect("primary capacity");
    let old_pool = Arc::clone(&pool);
    let old = tokio::spawn(async move {
        old_pool
            .select_server_wait("primary", QueueTag::default())
            .await
    });
    wait_for_queued(&pool, 1).await;
    tokio::time::sleep(Duration::from_millis(25)).await;
    let new_pool = Arc::clone(&pool);
    let new = tokio::spawn(async move { new_pool.select_wait().await });
    wait_for_queued(&pool, 2).await;

    assert!(old.await.expect("old waiter task").is_none());
    let acquired = new
        .await
        .expect("new waiter task")
        .expect("new waiter lease");
    assert_eq!(acquired.server_name(), "secondary");
    drop((primary, acquired));

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 1);
    assert_eq!(snapshot.queue_cancellations, 0);
}

#[tokio::test]
async fn reusable_notification_is_skipped_when_queueing_is_immutably_disabled() {
    let pool = RoundRobinPool::new_named_servers(
        "unbounded".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("unbounded pool");
    let lifetime = connection_lifetime(&pool, "only");
    assert!(lifetime.try_acquire().expect("initial acquisition"));

    lifetime.notify_reusable();

    assert_eq!(pool.queue.generation.load(Ordering::Acquire), 0);
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 0);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn bounded_runtime_override_without_queueing_cannot_create_a_waiter() {
    let pool = RoundRobinPool::new_named_servers(
        "immediate".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("immediate pool");
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    pool.set_server_max_connections("only", Some(1))
        .expect("bounded override");
    let notifications = pool.queue.notifications.load(Ordering::Relaxed);
    let generation = pool.queue.generation.load(Ordering::Acquire);
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));

    assert!(second.wait_for_capacity(generation).await.is_err());
    first.notify_reusable();

    assert_eq!(
        pool.queue.notifications.load(Ordering::Relaxed),
        notifications
    );
    assert_eq!(pool.queue.generation.load(Ordering::Acquire), generation);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn reusable_notification_wakes_a_hidden_bounded_lifetime_waiter() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "bounded".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("bounded pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();
    let waiting = Arc::clone(&second);
    let waiter = tokio::spawn(async move { waiting.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;
    let notifications = pool.queue.notifications.load(Ordering::Relaxed);

    first.notify_reusable();

    waiter
        .await
        .expect("waiter task")
        .expect("reusable notification");
    assert_eq!(
        pool.queue.notifications.load(Ordering::Relaxed),
        notifications + 1
    );
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn capacity_overrides_preserve_wakes_in_both_directions() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "overrides".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("override pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("unbounded acquisition"));
    first.notify_reusable();
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);

    pool.set_server_max_connections("only", Some(1))
        .expect("None to Some override");
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 2);
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("bounded acquisition"));
    let generation = second.capacity_generation();
    let waiting = Arc::clone(&second);
    let waiter = tokio::spawn(async move { waiting.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    pool.set_server_max_connections("only", None)
        .expect("Some to None override");

    waiter
        .await
        .expect("waiter task")
        .expect("override notification");
    assert!(second.try_acquire().expect("restored unbounded capacity"));
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 3);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn reusable_notification_before_waiter_registration_is_observed_by_generation() {
    let pool = RoundRobinPool::new_named_servers(
        "registration".into(),
        [runtime_server("only", 3000, Some(1))],
        UpstreamAlgorithm::First,
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("registration pool");
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();

    first.notify_reusable();

    tokio::time::timeout(
        Duration::from_millis(100),
        second.wait_for_capacity(generation),
    )
    .await
    .expect("generation change avoids a lost notification")
    .expect("generation notification");
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn cancelling_a_hidden_lifetime_waiter_releases_test_accounting_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "lifetime-cancel".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(10)),
        )
        .expect("cancellation pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();
    let waiter = tokio::spawn(async move { second.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    waiter.abort();
    assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
    wait_for_lifetime_waiters(&pool, 0).await;
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn capacity_generation_wrap_still_wakes_registered_waiters() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "generation-wrap".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("generation pool"),
    );
    pool.queue.generation.store(u64::MAX, Ordering::Release);
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();
    let waiter = tokio::spawn(async move { second.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    first.notify_reusable();

    waiter
        .await
        .expect("waiter task")
        .expect("wrapped generation notification");
    assert_eq!(pool.queue.generation.load(Ordering::Acquire), 0);
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

fn queued_pool(servers: &[(&str, Option<u64>)]) -> Arc<RoundRobinPool> {
    Arc::new(
        RoundRobinPool::new_named_servers(
            "queued".into(),
            servers
                .iter()
                .zip(3000..)
                .map(|((name, limit), port)| runtime_server(name, port, *limit)),
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(30)),
        )
        .expect("queued pool"),
    )
}

#[tokio::test]
async fn free_capacity_never_enters_the_wait_queue() {
    let pool = queued_pool(&[("only", Some(2))]);

    for _ in 0..3 {
        drop(pool.select_wait().await.expect("free capacity"));
    }

    assert_eq!(pool.queue.enqueued.load(Ordering::Relaxed), 0);
    assert_eq!(pool.health_snapshot().queued_total, 0);
}

#[tokio::test]
async fn a_saturated_pool_queues_and_leaves_the_queue_when_served() {
    let pool = queued_pool(&[("only", Some(1))]);
    let held = pool.select().expect("capacity");
    let waiting = Arc::clone(&pool);
    let waiter = tokio::spawn(async move { waiting.select_wait().await });
    wait_for_queued(&pool, 1).await;
    assert!(!pool.queue.is_unqueued());

    drop(held);
    drop(waiter.await.expect("task").expect("served"));

    assert!(pool.queue.is_unqueued());
    assert_eq!(pool.queue.enqueued.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn an_arrival_does_not_overtake_a_waiter_that_was_already_queued() {
    let pool = queued_pool(&[("only", Some(1))]);
    let held = pool.select().expect("capacity");
    let waiting = Arc::clone(&pool);
    let first = tokio::spawn(async move { waiting.select_wait().await });
    wait_for_queued(&pool, 1).await;

    drop(held);
    let arrival = tokio::time::timeout(Duration::ZERO, pool.select_wait()).await;

    assert!(arrival.is_err(), "the arrival must queue behind the waiter");
    let first = first
        .await
        .expect("task")
        .expect("the waiter is served first");
    assert_eq!(first.server_name(), "only");
}

#[tokio::test]
async fn many_waiters_are_served_in_arrival_order() {
    let pool = queued_pool(&[("only", Some(1))]);
    let held = pool.select().expect("capacity");
    let (served, mut order) = tokio::sync::mpsc::unbounded_channel();
    for index in 0..40_u64 {
        let (waiting, served) = (Arc::clone(&pool), served.clone());
        tokio::spawn(async move {
            let lease = waiting.select_wait().await.expect("served");
            served.send(index).expect("receiver");
            tokio::task::yield_now().await;
            drop(lease);
        });
        wait_for_queued(&pool, index + 1).await;
    }

    drop(held);
    for expected in 0..40 {
        assert_eq!(order.recv().await, Some(expected));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contending_requests_never_sleep_through_a_release() {
    let pool = queued_pool(&[("a", Some(1)), ("b", Some(1))]);
    let workers = (0..16)
        .map(|_| {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move {
                for _ in 0..300 {
                    let lease = pool.select_wait().await.expect("served before the timeout");
                    tokio::task::yield_now().await;
                    drop(lease);
                }
            })
        })
        .collect::<Vec<_>>();

    tokio::time::timeout(Duration::from_secs(20), async {
        for worker in workers {
            worker.await.expect("worker");
        }
    })
    .await
    .expect("every request was served without waiting for its queue timeout");

    let snapshot = pool.health_snapshot();
    assert_eq!((snapshot.queued, snapshot.queue_timeouts), (0, 0));
    assert!(pool.queue.is_unqueued());
}

fn reuse_pool(algorithm: UpstreamAlgorithm) -> Arc<EndpointPool> {
    reuse_pool_of(algorithm, &["a", "b"])
}

fn reuse_pool_of(algorithm: UpstreamAlgorithm, names: &[&str]) -> Arc<EndpointPool> {
    Arc::new(
        EndpointPool::new_named_servers(
            "reserve".into(),
            names
                .iter()
                .zip(3000..)
                .map(|(name, port)| runtime_server(name, port, Some(1))),
            algorithm,
            None,
            Some(Duration::from_secs(5)),
        )
        .expect("pool"),
    )
}

/// A pooled keep-alive connection of `name`: it holds the server's only permit and waits idle.
fn idle_connection(pool: &EndpointPool, name: &str) -> Arc<EndpointLeaseInner> {
    let connection = connection_lifetime(pool, name);
    assert!(connection.try_acquire().expect("permit"));
    connection.notify_reusable();
    connection
}

async fn reusing(pool: &EndpointPool) -> EndpointLease {
    pool.select_connection_target_waiting(&[], QueueTag::default())
        .await
        .expect("lease")
}

async fn nothing_is_free(pool: &EndpointPool) -> bool {
    tokio::time::timeout(Duration::from_millis(40), reusing(pool))
        .await
        .is_err()
}

#[tokio::test]
async fn a_selection_reserves_the_idle_connection_it_will_reuse() {
    let pool = reuse_pool(UpstreamAlgorithm::First);
    let _a = idle_connection(&pool, "a");
    let _b = idle_connection(&pool, "b");

    let first = reusing(&pool).await;
    let second = reusing(&pool).await;

    assert_eq!(first.server_name(), "a");
    assert_eq!(
        second.server_name(),
        "b",
        "a is reserved for the first request"
    );
}

#[tokio::test]
async fn a_request_that_finds_no_idle_connection_left_waits_for_a_release() {
    let pool = reuse_pool(UpstreamAlgorithm::First);
    let _a = idle_connection(&pool, "a");
    let _b = idle_connection(&pool, "b");
    let first = reusing(&pool).await;
    let _second = reusing(&pool).await;
    let third = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move { reusing(&pool).await.server_name().to_owned() }
    });
    super::queue::wait_for_queued(&pool, 1).await;

    drop(first);

    assert_eq!(third.await.expect("third"), "a");
}

#[tokio::test]
async fn a_reserved_connection_that_is_taken_stays_reserved_until_the_request_is_connected() {
    let pool = reuse_pool_of(UpstreamAlgorithm::First, &["a"]);
    let a = idle_connection(&pool, "a");
    let first = reusing(&pool).await;

    a.notify_in_use();
    assert!(nothing_is_free(&pool).await, "a serves the first request");
    first.end_queue_wait();
    a.notify_reusable();

    assert_eq!(reusing(&pool).await.server_name(), "a");
}

#[tokio::test]
async fn dropping_a_lease_that_never_connected_frees_its_reservation() {
    let pool = reuse_pool_of(UpstreamAlgorithm::First, &["a"]);
    let a = idle_connection(&pool, "a");
    let first = reusing(&pool).await;
    assert!(nothing_is_free(&pool).await);

    drop(first);

    assert_eq!(reusing(&pool).await.server_name(), "a");
    drop(a);
}
