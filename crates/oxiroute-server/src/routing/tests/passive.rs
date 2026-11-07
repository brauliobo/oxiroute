use super::*;

#[test]
fn unconfigured_passive_health_never_counts_or_ejects_failures() {
    let pool = RoundRobinPool::new_named_servers(
        "no-passive-policy".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("pool without passive policy");

    for _ in 0..10 {
        pool.record_passive_failure_at(0, HealthFailure::Timeout, now_unix_ms());
    }

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.available_endpoints, 1);
    assert_eq!(snapshot.unavailable_selections, 0);
    assert_eq!(snapshot.endpoints[0].passive_failure_count, 0);
    assert_eq!(snapshot.endpoints[0].passive_ejection_count, 0);
    assert!(!snapshot.endpoints[0].passive_ejected);
    assert!(pool.select().is_some());
}

#[test]
fn passive_ejection_is_deterministic_and_active_health_reentry_preserves_weighting() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-weighted".into(),
        [
            runtime_server("primary", 3000, None),
            runtime_server("backup", 3001, None),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1],
        },
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(2, Duration::from_mins(1), Duration::from_mins(2)),
    )
    .expect("passive policy pool");

    let base = now_unix_ms();
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, base);
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, base + 1);
    let ejected = pool.health_snapshot();
    assert_eq!(ejected.available_endpoints, 1);
    assert!(ejected.endpoints[0].passive_ejected);
    assert_eq!(ejected.endpoints[0].passive_failure_count, 2);
    assert_eq!(ejected.endpoints[0].passive_ejection_count, 1);
    assert_eq!(
        ejected.endpoints[0].passive_ejection_reason,
        Some(HealthFailure::ConnectFailed)
    );
    assert_eq!(
        ejected.endpoints[0].passive_ejected_at_unix_ms,
        Some(base + 1)
    );
    assert_eq!(
        ejected.endpoints[0].passive_ejection_until_unix_ms,
        Some(base + 60_001)
    );

    let selected_while_ejected = (0..16)
        .map(|_| {
            pool.select()
                .expect("backup remains eligible")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(selected_while_ejected.iter().all(|name| name == "backup"));

    pool.record_health(&pool.handle(0), true, None, Some(base + 10), 1, 1);
    let recovered = pool.health_snapshot();
    assert!(!recovered.endpoints[0].passive_ejected);
    assert_eq!(recovered.endpoints[0].passive_recovery_count, 1);
    assert_eq!(
        recovered.endpoints[0].passive_last_recovery_at_unix_ms,
        Some(base + 10)
    );

    let selected_after_recovery = (0..4)
        .map(|_| {
            pool.select()
                .expect("recovered endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected_after_recovery,
        ["primary", "primary", "primary", "backup"]
    );
}

#[test]
fn passive_ejection_backoff_is_bounded_and_active_failures_are_not_double_counted() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-backoff".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(1, Duration::from_millis(100), Duration::from_millis(250)),
    )
    .expect("passive backoff pool");
    let base = now_unix_ms();

    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::ConnectFailed),
        Some(base.saturating_sub(500)),
        1,
        1,
    );
    assert_eq!(pool.health_snapshot().endpoints[0].passive_failure_count, 0);

    pool.record_passive_failure_at(0, HealthFailure::Timeout, base);
    pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 101);
    pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 302);
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.endpoints[0].passive_failure_count, 3);
    assert_eq!(snapshot.endpoints[0].passive_ejection_count, 3);
    assert_eq!(
        snapshot.endpoints[0].passive_ejection_until_unix_ms,
        Some(base + 552)
    );
    assert!(snapshot.endpoints[0].passive_ejected);
}

#[test]
fn passive_ejection_fails_closed_without_revoking_concurrent_leases() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-leases".into(),
        [runtime_server("only", 3000, Some(2))],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(1)),
    )
    .expect("passive lease pool");
    let base = now_unix_ms();
    let first = pool.select().expect("first lease");
    let second = pool.select().expect("second lease");

    pool.record_passive_failure_at(0, HealthFailure::ProtocolError, base);
    assert!(pool.select().is_none(), "ejected pool must fail closed");
    assert_eq!(pool.health_snapshot().unavailable_selections, 1);
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 2);

    drop((first, second));
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 0);
}

#[test]
fn invalid_passive_policy_is_rejected_at_pool_construction() {
    let error = RoundRobinPool::new_named_servers_with_policy(
        "invalid-passive".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
        PassiveFailurePolicy::new(0, Duration::from_secs(1), Duration::from_secs(1)),
    )
    .expect_err("zero threshold must be rejected");
    assert!(matches!(error, PoolError::InvalidPassivePolicy { .. }));
}

#[test]
fn passive_policy_bounds_are_rejected_at_pool_construction() {
    let policies = [
        PassiveFailurePolicy::new(101, Duration::from_secs(1), Duration::from_secs(1)),
        PassiveFailurePolicy::new(1, Duration::ZERO, Duration::from_secs(1)),
        PassiveFailurePolicy::new(1, Duration::from_secs(2), Duration::from_secs(1)),
        PassiveFailurePolicy::new(
            1,
            Duration::from_secs(1),
            Duration::from_secs(24 * 60 * 60 + 1),
        ),
    ];

    for policy in policies {
        let error = RoundRobinPool::new_named_servers_with_policy(
            "invalid-passive".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            None,
            policy,
        )
        .expect_err("invalid passive policy must be rejected");
        assert!(matches!(error, PoolError::InvalidPassivePolicy { .. }));
    }
}

#[test]
fn passive_ejection_expiry_allows_new_selection() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-expiry".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(1, Duration::from_millis(100), Duration::from_millis(100)),
    )
    .expect("passive expiry pool");
    let now = now_unix_ms();
    pool.record_passive_failure_at(0, HealthFailure::Timeout, now.saturating_sub(101));

    assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
    assert_eq!(
        pool.select().expect("expired endpoint").server_name(),
        "only"
    );
}

#[test]
fn configured_passive_observation_filters_layer_seven_failures() {
    let mut policy = PassiveFailurePolicy::from_config(&oxiroute_config::PassiveHealthPolicy {
        observe: PassiveObserve::Layer4,
        on_error: PassiveOnError::Immediately,
        ..oxiroute_config::PassiveHealthPolicy::default()
    });
    policy.initial_ejection_duration = Duration::from_mins(1);
    policy.max_ejection_duration = Duration::from_mins(1);
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-observe".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("passive observe pool");

    pool.record_passive_failure_at(0, HealthFailure::ProtocolError, now_unix_ms());
    assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
    assert!(pool.health_snapshot().endpoints[0].passive_ejected);
}

#[test]
fn configured_mark_down_honors_the_error_limit_before_ejecting() {
    let policy = PassiveFailurePolicy::from_config(&oxiroute_config::PassiveHealthPolicy {
        on_error: PassiveOnError::MarkDown,
        error_limit: 2,
        ..oxiroute_config::PassiveHealthPolicy::default()
    });
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-mark-down".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("passive mark-down pool");

    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
    assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
    let endpoint = &pool.health_snapshot().endpoints[0];
    assert!(endpoint.passive_ejected);
    assert_eq!(endpoint.state, EndpointHealthState::Unhealthy);
}

#[test]
fn a_configured_passive_policy_applies_the_documented_defaults_to_omitted_fields() {
    let policy =
        PassiveFailurePolicy::from_config(&oxiroute_config::PassiveHealthPolicy::default());
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "documented-defaults".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("pool");
    let base = now_unix_ms();

    pool.record_passive_failure_at(0, HealthFailure::Timeout, base);
    pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 1);
    assert_eq!(pool.health_snapshot().available_endpoints, 1);
    pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 2);

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.available_endpoints, 0);
    assert_eq!(
        snapshot.endpoints[0].passive_ejection_until_unix_ms,
        Some(base + 2 + 30_000)
    );
    assert_eq!(policy.max_ejection_duration, Duration::from_mins(5));
    assert_eq!(policy.recovery_threshold, 1);
}
