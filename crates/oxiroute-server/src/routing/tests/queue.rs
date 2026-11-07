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
