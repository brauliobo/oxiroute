use super::*;

#[test]
fn selection_retries_across_a_pool_health_transition() {
    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
    ];
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "checked".into(),
            endpoints.map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool"),
    );
    pool.handle(0)
        .0
        .state
        .store(EndpointHealthState::Unhealthy as u8, Ordering::Relaxed);
    pool.handle(1)
        .0
        .state
        .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
    let writer = pool.health.health_writer.lock().expect("health writer");
    pool.health.health_version.store(1, Ordering::Release);
    let barrier = Arc::new(Barrier::new(2));
    let (selection_tx, selection_rx) = mpsc::channel();
    let selection_pool = Arc::clone(&pool);
    let selection_barrier = Arc::clone(&barrier);
    let selection_task = thread::spawn(move || {
        selection_barrier.wait();
        selection_tx
            .send(
                selection_pool
                    .select()
                    .map(|lease| lease.endpoint().clone()),
            )
            .expect("selection receiver");
    });
    barrier.wait();
    assert!(
        selection_rx
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );

    pool.handle(0)
        .0
        .state
        .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
    pool.handle(1)
        .0
        .state
        .store(EndpointHealthState::Unhealthy as u8, Ordering::Relaxed);
    pool.health.health_version.store(2, Ordering::Release);
    drop(writer);

    assert_eq!(
        selection_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stable selection"),
        Some(RuntimeEndpoint::from(endpoints[0]))
    );
    selection_task.join().expect("selection task");
    assert_eq!(pool.health_snapshot().unavailable_selections, 0);
}

#[test]
fn health_aware_selection_is_fair_across_available_endpoints() {
    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
        SocketAddr::from(([127, 0, 0, 1], 3002)),
    ];
    let pool = RoundRobinPool::new_named(
        "checked".into(),
        endpoints.map(RuntimeEndpoint::from),
        UpstreamAlgorithm::RoundRobin,
        true,
    )
    .expect("checked pool");
    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::ConnectFailed),
        Some(100),
        1,
        1,
    );
    pool.record_health(&pool.handle(1), true, None, Some(100), 1, 1);
    pool.record_health(&pool.handle(2), true, None, Some(100), 1, 1);

    let selected = (0..6)
        .map(|_| pool.select().map(|lease| lease.endpoint().clone()))
        .collect::<Vec<_>>();
    let endpoints = endpoints.map(RuntimeEndpoint::from);
    assert_eq!(
        selected,
        vec![
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
        ]
    );
}

#[test]
fn weighted_round_robin_is_deterministic_and_exposes_effective_weights() {
    let pool = RoundRobinPool::new_named_servers(
        "weighted".into(),
        [
            runtime_server("primary", 3000, None),
            runtime_server("backup", 3001, None),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1],
        },
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("weighted pool");

    let selected = (0..8)
        .map(|_| {
            pool.select()
                .expect("weighted endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected,
        [
            "primary", "primary", "backup", "primary", "primary", "primary", "backup", "primary",
        ]
    );

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.algorithm, "weighted_round_robin");
    assert_eq!(
        snapshot
            .endpoints
            .iter()
            .map(|endpoint| endpoint.weight)
            .collect::<Vec<_>>(),
        vec![3, 1]
    );
}

#[test]
fn weighted_round_robin_skips_unhealthy_endpoints_and_recovers_them() {
    let pool = RoundRobinPool::new_named_servers(
        "weighted-health".into(),
        [
            runtime_server("primary", 3000, None),
            runtime_server("backup", 3001, None),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1],
        },
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("weighted health pool");
    pool.record_health(
        &pool.handle(1),
        false,
        Some(HealthFailure::ConnectFailed),
        Some(100),
        1,
        1,
    );

    let unavailable = (0..4)
        .map(|_| {
            pool.select()
                .expect("healthy endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(unavailable, ["primary", "primary", "primary", "primary"]);
    assert_eq!(
        pool.health_snapshot().endpoints[1].state,
        EndpointHealthState::Unhealthy
    );

    pool.record_health(&pool.handle(1), true, None, Some(200), 1, 1);
    let recovered = (0..4)
        .map(|_| {
            pool.select()
                .expect("recovered endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(recovered, ["primary", "primary", "backup", "primary"]);
    assert_eq!(
        pool.health_snapshot().endpoints[1].state,
        EndpointHealthState::Healthy
    );
}

#[test]
fn weighted_round_robin_leases_respect_capacity_and_release() {
    let pool = RoundRobinPool::new_named_servers(
        "weighted-capacity".into(),
        [
            runtime_server("primary", 3000, Some(1)),
            runtime_server("backup", 3001, Some(1)),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![2, 1],
        },
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("weighted capacity pool");

    let primary = pool.select().expect("primary lease");
    assert_eq!(primary.server_name(), "primary");
    let backup = pool.select().expect("backup lease");
    assert_eq!(backup.server_name(), "backup");
    assert!(pool.select().is_none(), "both endpoints are at capacity");
    drop(primary);
    assert_eq!(
        pool.select().expect("released primary").server_name(),
        "primary"
    );
    drop(backup);
}

#[test]
fn concurrent_health_aware_selection_distributes_every_available_turn() {
    const THREADS: usize = 8;
    const SELECTIONS_PER_THREAD: usize = 250;

    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
        SocketAddr::from(([127, 0, 0, 1], 3002)),
    ];
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "checked".into(),
            endpoints.map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool"),
    );
    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::ConnectFailed),
        Some(100),
        1,
        1,
    );
    pool.record_health(&pool.handle(1), true, None, Some(100), 1, 1);
    pool.record_health(&pool.handle(2), true, None, Some(100), 1, 1);

    let selected = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                (0..SELECTIONS_PER_THREAD)
                    .map(|_| {
                        pool.select()
                            .expect("available endpoint")
                            .endpoint()
                            .clone()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|task| task.join().expect("selection thread"))
        .collect::<Vec<_>>();

    let endpoints = endpoints.map(RuntimeEndpoint::from);
    assert!(!selected.contains(&endpoints[0]));
    for endpoint in &endpoints[1..] {
        assert_eq!(
            selected
                .iter()
                .filter(|selected| *selected == endpoint)
                .count(),
            THREADS * SELECTIONS_PER_THREAD / 2
        );
    }
}

#[test]
fn concurrent_weighted_selection_preserves_cycle_fairness() {
    const THREADS: usize = 4;
    const SELECTIONS_PER_THREAD: usize = 100;

    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "weighted-concurrent".into(),
            [
                runtime_server("primary", 3000, None),
                runtime_server("backup", 3001, None),
            ],
            UpstreamAlgorithm::WeightedRoundRobin {
                weights: vec![3, 1],
            },
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("weighted concurrent pool"),
    );
    let selected = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                (0..SELECTIONS_PER_THREAD)
                    .map(|_| {
                        pool.select()
                            .expect("weighted endpoint")
                            .server_name()
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|task| task.join().expect("weighted selection thread"))
        .collect::<Vec<_>>();

    assert_eq!(
        selected
            .iter()
            .filter(|name| name.as_str() == "primary")
            .count(),
        THREADS * SELECTIONS_PER_THREAD * 3 / 4
    );
    assert_eq!(
        selected
            .iter()
            .filter(|name| name.as_str() == "backup")
            .count(),
        THREADS * SELECTIONS_PER_THREAD / 4
    );
}
