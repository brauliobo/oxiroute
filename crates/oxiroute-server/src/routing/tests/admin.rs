use super::*;

#[test]
fn administrative_drain_rejects_new_work_without_revoking_existing_leases() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, Some(2))],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    let lease = pool.select().expect("existing lease");

    pool.set_server_administrative_state("one", AdministrativeState::Drain)
        .expect("drain");

    assert!(pool.select().is_none());
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 1);
    drop(lease);
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 0);
}

#[test]
fn administrative_state_moves_to_another_pool_by_pool_and_server_name() {
    let pool = |servers: &[&str]| {
        RoundRobinPool::new_named_servers(
            "api".into(),
            servers
                .iter()
                .zip(3000..)
                .map(|(name, port)| runtime_server(name, port, None)),
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool")
    };
    let previous = pool(&["one", "two", "gone"]);
    assert!(previous.administrative_states().is_empty());
    previous
        .set_server_administrative_state("one", AdministrativeState::Drain)
        .expect("drain");
    previous
        .set_server_health_override("one", HealthOverride::Down)
        .expect("override");
    previous
        .set_server_max_connections("one", Some(4))
        .expect("limit");
    previous
        .set_server_checks_enabled("one", false)
        .expect("checks");
    previous
        .set_server_administrative_state("gone", AdministrativeState::Maintenance)
        .expect("maintenance");

    let next = pool(&["one", "two", "new"]);
    next.restore_administrative_states(&previous.administrative_states());

    let states = next.administrative_states();
    assert_eq!(states.len(), 1, "{states:?}");
    assert_eq!(states[0].server, "one");
    assert_eq!(states[0].administrative, AdministrativeState::Drain);
    assert_eq!(states[0].health, HealthOverride::Down);
    assert_eq!(states[0].max_connections, Some(4));
    assert!(!states[0].checks_enabled);
    let snapshot = next.health_snapshot();
    assert_eq!(snapshot.endpoints[0].max_connections, Some(4));
    assert_eq!(
        snapshot.endpoints[2].administrative_state,
        AdministrativeState::Ready
    );
    let other_pool = RoundRobinPool::new_named_servers(
        "web".into(),
        [runtime_server("one", 3000, None)],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    other_pool.restore_administrative_states(&previous.administrative_states());
    assert!(other_pool.administrative_states().is_empty());
}

#[test]
fn maintenance_suspends_checks_while_drain_keeps_checks_running() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, None)],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");

    pool.set_server_administrative_state("one", AdministrativeState::Drain)
        .expect("drain");
    assert!(pool.health_checks_running(0));
    pool.set_server_administrative_state("one", AdministrativeState::Maintenance)
        .expect("maintenance");
    assert!(!pool.health_checks_running(0));
    pool.set_server_checks_enabled("one", false)
        .expect("disable checks");
    pool.set_server_administrative_state("one", AdministrativeState::Ready)
        .expect("ready");
    assert!(!pool.health_checks_running(0));
}

#[test]
fn health_override_is_independent_from_observed_health_and_resets_to_auto() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, None)],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    pool.record_health(0, false, Some(HealthFailure::ConnectFailed), Some(1), 1, 1);
    assert!(pool.select().is_none());

    pool.set_server_health_override("one", HealthOverride::Up)
        .expect("force up");
    assert!(pool.select().is_some());
    let snapshot = pool.health_snapshot().endpoints.remove(0);
    assert_eq!(snapshot.state, EndpointHealthState::Unhealthy);
    assert_eq!(snapshot.health_override, HealthOverride::Up);

    pool.set_server_health_override("one", HealthOverride::Auto)
        .expect("automatic health");
    assert!(pool.select().is_none());
}

#[test]
fn max_connections_override_and_reset_preserve_configured_capacity() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, Some(2))],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    pool.set_server_max_connections("one", Some(1))
        .expect("override");
    let first = pool.select().expect("first");
    assert!(pool.select().is_none());
    drop(first);

    pool.set_server_max_connections("one", None).expect("reset");
    let first = pool.select().expect("first after reset");
    let second = pool.select().expect("configured second capacity");
    assert_eq!(pool.health_snapshot().endpoints[0].max_connections, Some(2));
    drop((first, second));
}

#[test]
fn first_uses_the_first_healthy_administrative_server_with_capacity() {
    let pool = RoundRobinPool::new_named_servers(
        "first".into(),
        [
            runtime_server("primary", 3000, Some(1)),
            runtime_server("backup", 3001, Some(1)),
        ],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("first pool");

    let primary = pool.select().expect("primary capacity");
    assert_eq!(primary.server_name(), "primary");
    let backup = pool.select().expect("backup capacity");
    assert_eq!(backup.server_name(), "backup");
    drop(primary);
    assert_eq!(
        pool.select().expect("primary restored").server_name(),
        "primary"
    );
    drop(backup);

    pool.record_health(0, false, Some(HealthFailure::ConnectFailed), Some(1), 1, 1);
    assert_eq!(
        pool.select().expect("healthy backup").server_name(),
        "backup"
    );
}

#[test]
fn least_connections_uses_named_server_work_and_rotates_equal_ties() {
    let pool = RoundRobinPool::new_named_servers(
        "least".into(),
        [
            runtime_server("one", 3000, Some(2)),
            runtime_server("two", 3001, Some(2)),
            runtime_server("three", 3002, Some(2)),
        ],
        UpstreamAlgorithm::LeastConnections,
        None,
        None,
    )
    .expect("least-connections pool");

    let one = pool.select().expect("one");
    let two = pool.select().expect("two");
    let three = pool.select().expect("three");
    assert_eq!(
        [one.server_name(), two.server_name(), three.server_name()],
        ["one", "two", "three"]
    );
    assert_eq!(
        pool.health_snapshot()
            .endpoints
            .iter()
            .map(|server| (server.name.as_str(), server.active_connections))
            .collect::<Vec<_>>(),
        vec![("one", 1), ("two", 1), ("three", 1)]
    );
}
