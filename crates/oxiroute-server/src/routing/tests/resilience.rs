use super::*;

fn fail_open_pool(servers: &[(&str, Option<u64>)], algorithm: UpstreamAlgorithm) -> EndpointPool {
    EndpointPool::new_named_servers_resilient(
        "fail-open".into(),
        servers
            .iter()
            .zip(3000..)
            .map(|((name, max), port)| runtime_server(name, port, *max)),
        algorithm,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(2)),
        PoolResiliencePolicy {
            fail_open: true,
            min_members: 0,
            ..PoolResiliencePolicy::default()
        },
    )
    .expect("fail-open pool")
}

fn mark_unhealthy(pool: &EndpointPool, index: usize, failed_checks: u64) {
    for _ in 0..failed_checks {
        pool.record_health(
            index,
            false,
            Some(HealthFailure::UnexpectedStatus),
            None,
            1,
            1,
        );
    }
}

#[test]
fn fail_open_is_off_by_default_so_unhealthy_pools_stay_unavailable() {
    let pool = RoundRobinPool::new_named_servers(
        "closed".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    mark_unhealthy(&pool, 0, 1);

    assert!(pool.select().is_none());
    assert!(pool.select_connection_target_excluding(&[]).is_none());
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.unavailable_selections, 2);
    assert_eq!(snapshot.fail_open_selections, 0);
}

#[test]
fn fail_open_serves_the_only_unhealthy_member() {
    let pool = fail_open_pool(&[("only", None)], UpstreamAlgorithm::First);
    mark_unhealthy(&pool, 0, 3);

    let lease = pool.select().expect("fail-open lease");

    assert_eq!(lease.server_name(), "only");
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.available_endpoints, 0);
    assert_eq!(snapshot.unavailable_selections, 0);
    assert_eq!(snapshot.fail_open_selections, 1);
    assert_eq!(snapshot.endpoints[0].active_connections, 1);
}

#[test]
fn fail_open_leaves_normal_selection_alone_while_a_member_is_selectable() {
    let pool = fail_open_pool(&[("a", None), ("b", None)], UpstreamAlgorithm::First);
    mark_unhealthy(&pool, 0, 1);

    assert_eq!(pool.select().expect("lease").server_name(), "b");
    assert_eq!(pool.health_snapshot().fail_open_selections, 0);
}

#[test]
fn fail_open_prefers_healthy_ejected_soonest_then_fewest_failed_checks() {
    let pool = fail_open_pool(
        &[
            ("flaky", None),
            ("down", None),
            ("ejected-late", None),
            ("ejected-soon", None),
        ],
        UpstreamAlgorithm::RoundRobin,
    );
    let base = now_unix_ms();
    mark_unhealthy(&pool, 0, 2);
    mark_unhealthy(&pool, 1, 5);
    pool.record_passive_failure_at(2, HealthFailure::Timeout, base + 5_000);
    pool.record_passive_failure_at(3, HealthFailure::Timeout, base);

    let order = (1..=4)
        .scan(Vec::<String>::new(), |excluded, _| {
            let lease = pool.select_server_excluding(excluded).lease?;
            excluded.push(lease.server_name().to_owned());
            Some(lease.server_name().to_owned())
        })
        .collect::<Vec<_>>();

    assert_eq!(order, ["ejected-soon", "ejected-late", "flaky", "down"]);
    assert_eq!(pool.health_snapshot().fail_open_selections, 4);
}

#[test]
fn fail_open_honours_drain_maintenance_and_forced_down() {
    let pool = fail_open_pool(
        &[
            ("drained", None),
            ("maintained", None),
            ("forced", None),
            ("sick", None),
        ],
        UpstreamAlgorithm::First,
    );
    for index in 0..4 {
        mark_unhealthy(&pool, index, 1);
    }
    pool.set_server_administrative_state("drained", AdministrativeState::Drain)
        .expect("drain");
    pool.set_server_administrative_state("maintained", AdministrativeState::Maintenance)
        .expect("maintenance");
    pool.set_server_health_override("forced", HealthOverride::Down)
        .expect("override");

    let lease = pool.select().expect("only the sick member is eligible");
    assert_eq!(lease.server_name(), "sick");
    drop(lease);

    pool.set_server_administrative_state("sick", AdministrativeState::Drain)
        .expect("drain");
    assert!(pool.select().is_none());
    assert_eq!(pool.health_snapshot().unavailable_selections, 1);
}

#[test]
fn fail_open_respects_max_connections() {
    let pool = fail_open_pool(&[("only", Some(1))], UpstreamAlgorithm::First);
    mark_unhealthy(&pool, 0, 1);

    let held = pool.select().expect("first lease");

    assert!(pool.select().is_none(), "capacity still applies");
    drop(held);
    assert!(pool.select().is_some());
}

#[tokio::test]
async fn fail_open_queues_for_capacity_like_a_healthy_pool() {
    let pool = Arc::new(
        EndpointPool::new_named_servers_resilient(
            "fail-open-queue".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Healthy),
            Some(Duration::from_secs(2)),
            PassiveFailurePolicy::default(),
            PoolResiliencePolicy {
                fail_open: true,
                min_members: 0,
                ..PoolResiliencePolicy::default()
            },
        )
        .expect("pool"),
    );
    mark_unhealthy(&pool, 0, 1);
    let held = pool.select_wait().await.expect("first lease");
    let waiter = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move {
            pool.select_wait()
                .await
                .map(|lease| lease.server_name().to_owned())
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    drop(held);

    assert_eq!(waiter.await.expect("waiter").as_deref(), Some("only"));
}

#[test]
fn fail_open_connection_targets_acquire_capacity_while_failing_open() {
    let pool = fail_open_pool(&[("only", Some(1))], UpstreamAlgorithm::First);
    mark_unhealthy(&pool, 0, 1);

    let lease = pool
        .select_connection_target_excluding(&[])
        .expect("pending lease");
    let lifetime = Arc::clone(&lease.inner);

    assert!(lifetime.try_acquire().expect("acquire"));
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 1);
    assert_eq!(pool.health_snapshot().fail_open_selections, 1);
    assert!(pool.select_server_connection_target("only").is_some());
}

#[test]
fn fail_open_counts_a_server_as_unattempted_only_once() {
    let pool = fail_open_pool(&[("a", None), ("b", None)], UpstreamAlgorithm::First);
    mark_unhealthy(&pool, 0, 1);
    mark_unhealthy(&pool, 1, 1);

    assert!(pool.has_unattempted_servers(&["a".to_owned()]));
    assert!(!pool.has_unattempted_servers(&["a".to_owned(), "b".to_owned()]));
    let closed = RoundRobinPool::new_named_servers(
        "closed".into(),
        [runtime_server("a", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    mark_unhealthy(&closed, 0, 1);
    assert!(!closed.has_unattempted_servers(&[]));
}

fn resilient_pool(
    servers: usize,
    resilience: PoolResiliencePolicy,
    passive: PassiveFailurePolicy,
) -> EndpointPool {
    EndpointPool::new_named_servers_resilient(
        "ejection".into(),
        (0..servers)
            .zip(3000..)
            .map(|(index, port)| runtime_server(&index.to_string(), port, None)),
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
        passive,
        resilience,
    )
    .expect("pool")
}

fn eject_once(pool: &EndpointPool, index: usize) {
    pool.record_passive_failure_at(index, HealthFailure::Timeout, now_unix_ms());
}

#[test]
fn passive_ejection_keeps_the_last_selectable_server_of_a_fail_open_pool() {
    let pool = resilient_pool(
        2,
        PoolResiliencePolicy::from_config(&oxiroute_config::PoolResilience {
            fail_open: true,
            ..oxiroute_config::PoolResilience::default()
        }),
        PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(2)),
    );

    eject_once(&pool, 0);
    eject_once(&pool, 1);
    eject_once(&pool, 1);

    let snapshot = pool.health_snapshot();
    assert!(snapshot.endpoints[0].passive_ejected);
    assert!(!snapshot.endpoints[1].passive_ejected);
    assert_eq!(snapshot.endpoints[1].passive_ejection_count, 0);
    assert_eq!(snapshot.endpoints[1].passive_failure_count, 2);
    assert_eq!(snapshot.available_endpoints, 1);
    assert_eq!(pool.select().expect("last server").server_name(), "1");
    assert_eq!(snapshot.fail_open_selections, 0);
}

#[test]
fn passive_ejection_may_empty_a_pool_unless_a_floor_is_configured() {
    let passive = PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(2));
    let unconfigured = resilient_pool(2, PoolResiliencePolicy::default(), passive);
    eject_once(&unconfigured, 0);
    eject_once(&unconfigured, 1);
    assert_eq!(unconfigured.health_snapshot().available_endpoints, 0);

    let zero_floor = resilient_pool(
        2,
        PoolResiliencePolicy::from_config(&oxiroute_config::PoolResilience {
            fail_open: true,
            min_members: Some(0),
            ..oxiroute_config::PoolResilience::default()
        }),
        passive,
    );
    eject_once(&zero_floor, 0);
    eject_once(&zero_floor, 1);
    assert_eq!(zero_floor.health_snapshot().available_endpoints, 0);
    assert!(zero_floor.select().is_some(), "fail-open still serves");
}

#[test]
fn min_members_keeps_that_many_servers_selectable() {
    let pool = resilient_pool(
        3,
        PoolResiliencePolicy::from_config(&oxiroute_config::PoolResilience {
            min_members: Some(2),
            ..oxiroute_config::PoolResilience::default()
        }),
        PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(2)),
    );

    for index in 0..3 {
        eject_once(&pool, index);
    }

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.available_endpoints, 2);
    assert_eq!(snapshot.endpoints[0].passive_ejection_count, 1);
    assert_eq!(snapshot.endpoints[1].passive_ejection_count, 0);
    assert_eq!(snapshot.endpoints[2].passive_ejection_count, 0);
}

#[test]
fn ejection_floor_counts_only_other_selectable_servers() {
    let pool = resilient_pool(
        2,
        PoolResiliencePolicy::from_config(&oxiroute_config::PoolResilience {
            min_members: Some(1),
            ..oxiroute_config::PoolResilience::default()
        }),
        PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(2)),
    );
    mark_unhealthy(&pool, 0, 1);

    eject_once(&pool, 1);

    assert_eq!(
        pool.health_snapshot().endpoints[1].passive_ejection_count,
        0
    );
}

#[test]
fn ejection_duration_jitter_stays_within_its_percentage_and_the_maximum() {
    let mut policy = PassiveFailurePolicy::new(1, Duration::from_secs(10), Duration::from_secs(25));
    policy.ejection_jitter_percent = 20;

    assert_eq!(policy.ejection_duration_for(1, 0), Duration::from_secs(8));
    assert_eq!(
        policy.ejection_duration_for(1, 2_000),
        Duration::from_secs(10)
    );
    assert_eq!(
        policy.ejection_duration_for(1, 4_000),
        Duration::from_secs(12)
    );
    assert_eq!(
        policy.ejection_duration_for(2, 8_000),
        Duration::from_secs(24)
    );
    assert_eq!(
        policy.ejection_duration_for(3, 10_000),
        Duration::from_secs(25)
    );
    policy.ejection_jitter_percent = 0;
    assert_eq!(policy.ejection_duration_for(1, 1), Duration::from_secs(10));
}

#[test]
fn jittered_ejections_do_not_all_expire_together() {
    let mut passive = PassiveFailurePolicy::new(1, Duration::from_secs(10), Duration::from_mins(1));
    passive.ejection_jitter_percent = 50;
    let pool = resilient_pool(16, PoolResiliencePolicy::default(), passive);
    let at = now_unix_ms();

    for index in 0..16 {
        pool.record_passive_failure_at(index, HealthFailure::Timeout, at);
    }

    let until = pool
        .health_snapshot()
        .endpoints
        .iter()
        .map(|endpoint| endpoint.passive_ejection_until_unix_ms.expect("ejected") - at)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(until.len() > 1, "{until:?}");
    assert!(
        until.iter().all(|ms| (5_000..=15_000).contains(ms)),
        "{until:?}"
    );
}

#[tokio::test]
async fn requests_wait_for_a_recovering_server_instead_of_failing_at_once() {
    for queue_timeout in [None, Some(Duration::from_secs(5))] {
        let pool = waiting_pool(Duration::from_secs(5), queue_timeout);
        let waiter = tokio::spawn({
            let pool = Arc::clone(&pool);
            async move {
                pool.select_wait()
                    .await
                    .map(|lease| lease.server_name().to_owned())
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiter.is_finished());

        pool.record_health(0, true, None, None, 1, 1);

        assert_eq!(waiter.await.expect("waiter").as_deref(), Some("only"));
        assert_eq!(pool.health_snapshot().unavailable_selections, 0);
    }
}

#[tokio::test]
async fn connection_targets_wait_for_a_recovering_server() {
    let pool = waiting_pool(Duration::from_secs(5), None);
    let waiter = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move { pool.select_connection_target_waiting(&[]).await.is_some() }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    pool.record_health(0, true, None, None, 1, 1);

    assert!(waiter.await.expect("waiter"));
}

#[tokio::test]
async fn the_wait_for_a_recovering_server_is_bounded_and_counted_once() {
    let pool = waiting_pool(Duration::from_millis(300), None);

    assert!(pool.select_wait().await.is_none());
    assert!(pool.select_connection_target_waiting(&[]).await.is_none());

    assert_eq!(pool.health_snapshot().unavailable_selections, 2);
}

#[tokio::test]
async fn pools_without_a_recovery_wait_fail_at_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "strict".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Checking),
            None,
        )
        .expect("pool"),
    );
    let started = Instant::now();

    assert!(pool.select_connection_target_waiting(&[]).await.is_none());
    assert!(pool.select_wait().await.is_none());

    assert!(started.elapsed() < Duration::from_millis(100));
}
