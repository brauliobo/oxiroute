use super::*;

#[test]
fn checked_endpoints_transition_through_thresholds() {
    let pool = RoundRobinPool::new_named(
        "checked".into(),
        [RuntimeEndpoint::from(SocketAddr::from((
            [127, 0, 0, 1],
            3000,
        )))],
        UpstreamAlgorithm::RoundRobin,
        true,
    )
    .expect("checked pool");

    assert!(pool.select().is_none());
    assert_eq!(
        pool.health_snapshot().endpoints[0].state,
        EndpointHealthState::Unknown
    );

    pool.record_health(&pool.handle(0), true, None, Some(100), 2, 2);
    assert!(
        !pool.has_available(),
        "one success remains below startup threshold"
    );
    pool.record_health(&pool.handle(0), true, None, Some(150), 2, 2);
    assert!(
        pool.has_available(),
        "the startup threshold establishes state"
    );
    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::ConnectFailed),
        Some(200),
        2,
        2,
    );
    assert!(pool.has_available(), "one failure remains below threshold");
    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::ConnectFailed),
        Some(300),
        2,
        2,
    );
    assert!(
        !pool.has_available(),
        "the failure threshold removes the endpoint"
    );
    pool.record_health(&pool.handle(0), true, None, Some(400), 2, 2);
    assert!(
        !pool.has_available(),
        "one success remains below recovery threshold"
    );
    pool.record_health(&pool.handle(0), true, None, Some(500), 2, 2);
    assert!(
        pool.has_available(),
        "the recovery threshold restores the endpoint"
    );

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.available_endpoints, 1);
    assert_eq!(snapshot.endpoints[0].successful_checks, 4);
    assert_eq!(snapshot.endpoints[0].failed_checks, 2);
    assert_eq!(snapshot.endpoints[0].last_transition_at_unix_ms, Some(500));
    assert_eq!(snapshot.endpoints[0].last_failure, None);
}

#[test]
fn unchecked_endpoints_remain_selectable_without_observations() {
    let endpoint = SocketAddr::from(([127, 0, 0, 1], 3000));
    let pool = RoundRobinPool::new([endpoint]).expect("unchecked pool");

    assert_eq!(
        pool.select().map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(endpoint))
    );
    assert_eq!(
        pool.health_snapshot().endpoints[0].state,
        EndpointHealthState::Unchecked
    );
}

#[test]
fn excluding_every_available_endpoint_does_not_count_pool_unavailability() {
    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
    ];
    let pool = RoundRobinPool::new(endpoints).expect("unchecked pool");
    let endpoints = endpoints.map(RuntimeEndpoint::from);

    assert!(pool.select_excluding(&endpoints).is_none());
    assert_eq!(pool.health_snapshot().unavailable_selections, 0);
}

#[test]
fn health_counters_serialize_without_losing_u64_precision() {
    let pool = RoundRobinPool::new_named(
        "checked".into(),
        [RuntimeEndpoint::from(SocketAddr::from((
            [127, 0, 0, 1],
            3000,
        )))],
        UpstreamAlgorithm::RoundRobin,
        true,
    )
    .expect("checked pool");
    pool.unavailable_selections
        .store(u64::MAX, Ordering::Relaxed);
    pool.handle(0)
        .0
        .active_work
        .store(u64::MAX, Ordering::Relaxed);
    pool.handle(0)
        .0
        .successful_checks
        .store(u64::MAX, Ordering::Relaxed);
    pool.handle(0)
        .0
        .failed_checks
        .store(u64::MAX, Ordering::Relaxed);
    pool.handle(0)
        .0
        .consecutive_successes
        .store(u64::MAX, Ordering::Relaxed);
    pool.handle(0)
        .0
        .consecutive_failures
        .store(u64::MAX, Ordering::Relaxed);

    let json = serde_json::to_value(pool.health_snapshot()).expect("health snapshot JSON");
    let exact = u64::MAX.to_string();
    assert_eq!(json["unavailableSelections"], exact);
    assert_eq!(json["endpoints"][0]["activeConnections"], exact);
    assert_eq!(json["endpoints"][0]["successfulChecks"], exact);
    assert_eq!(json["endpoints"][0]["failedChecks"], exact);
    assert_eq!(json["endpoints"][0]["consecutiveSuccesses"], exact);
    assert_eq!(json["endpoints"][0]["consecutiveFailures"], exact);
}

#[test]
fn health_snapshot_waits_for_a_complete_observation() {
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "checked".into(),
            [RuntimeEndpoint::from(SocketAddr::from((
                [127, 0, 0, 1],
                3000,
            )))],
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool"),
    );
    let writer = pool.health.health_writer.lock().expect("health writer");
    pool.health.health_version.store(1, Ordering::Release);
    let barrier = Arc::new(Barrier::new(2));
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    let snapshot_pool = Arc::clone(&pool);
    let snapshot_barrier = Arc::clone(&barrier);
    let snapshot_task = thread::spawn(move || {
        snapshot_barrier.wait();
        snapshot_tx
            .send(snapshot_pool.health_snapshot())
            .expect("snapshot receiver");
    });
    barrier.wait();
    assert!(snapshot_rx.recv_timeout(Duration::from_millis(20)).is_err());

    pool.handle(0)
        .0
        .state
        .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
    pool.handle(0)
        .0
        .last_checked_at_unix_ms
        .store(100, Ordering::Relaxed);
    pool.handle(0)
        .0
        .last_transition_at_unix_ms
        .store(100, Ordering::Relaxed);
    pool.handle(0)
        .0
        .successful_checks
        .store(1, Ordering::Relaxed);
    pool.health.health_version.store(2, Ordering::Release);
    drop(writer);

    let snapshot = snapshot_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("complete snapshot");
    snapshot_task.join().expect("snapshot task");
    assert_eq!(snapshot.endpoints[0].state, EndpointHealthState::Healthy);
    assert_eq!(snapshot.endpoints[0].last_checked_at_unix_ms, Some(100));
    assert_eq!(snapshot.endpoints[0].last_transition_at_unix_ms, Some(100));
    assert_eq!(snapshot.endpoints[0].successful_checks, 1);
}
