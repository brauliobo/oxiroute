use super::*;
use crate::routing::carry::fit_health_carry;

fn policy(breaker: bool) -> PassiveFailurePolicy {
    let mut policy = PassiveFailurePolicy::new(2, Duration::from_mins(1), Duration::from_mins(2));
    if breaker {
        policy.circuit_breaker = Some(CircuitBreakerPolicy::from(
            &oxiroute_config::CircuitBreaker {
                window_ms: 10_000,
                min_requests: 4,
                failure_ratio_percent: 50,
                open_ms: 60_000,
                half_open_max: 1,
                half_open_successes: 3,
            },
        ));
    }
    policy
}

fn pool(ports: [u16; 2], breaker: bool) -> RoundRobinPool {
    RoundRobinPool::new_named_servers_with_policy(
        "carried".into(),
        [
            runtime_server("first", ports[0], None),
            runtime_server("second", ports[1], None),
        ],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Checking),
        None,
        policy(breaker),
    )
    .expect("pool")
}

fn first_server(pool: &RoundRobinPool) -> Option<String> {
    pool.select().map(|lease| lease.server_name().to_owned())
}

fn first_state(pool: &RoundRobinPool) -> EndpointHealthSnapshot {
    pool.health_snapshot().endpoints.remove(0)
}

#[test]
fn a_replacement_pool_keeps_the_health_and_the_ejection_of_an_unchanged_server() {
    let old = pool([3000, 3001], false);
    let base = now_unix_ms();
    old.record_health(&old.handle(0), true, None, Some(base), 1, 1);
    old.record_health(&old.handle(1), true, None, Some(base), 1, 1);
    old.record_passive_failure_at(0, HealthFailure::ConnectFailed, base);
    old.record_passive_failure_at(0, HealthFailure::ConnectFailed, base + 1);
    let new = pool([3000, 3001], false);
    assert_eq!(first_state(&new).state, EndpointHealthState::Unknown);

    new.restore_health(&[old.health_carry()]);

    let restored = new.health_snapshot().endpoints;
    assert_eq!(restored[0].state, EndpointHealthState::Healthy);
    assert_eq!(restored[0].last_checked_at_unix_ms, Some(base));
    assert!(restored[0].passive_ejected);
    assert_eq!(restored[0].passive_ejection_count, 1);
    assert_eq!(restored[0].passive_ejected_at_unix_ms, Some(base + 1));
    assert_eq!(
        restored[0].passive_ejection_until_unix_ms,
        Some(base + 60_001)
    );
    assert_eq!(restored[1].state, EndpointHealthState::Healthy);
    assert_eq!(new.health_snapshot().available_endpoints, 1);
}

#[test]
fn a_server_found_unhealthy_stays_unhealthy_in_the_replacement_pool() {
    let old = pool([3000, 3001], false);
    let base = now_unix_ms();
    old.record_health(
        &old.handle(0),
        false,
        Some(HealthFailure::Timeout),
        Some(base),
        1,
        1,
    );
    let new = pool([3000, 3001], false);

    new.restore_health(&[old.health_carry()]);

    let restored = first_state(&new);
    assert_eq!(restored.state, EndpointHealthState::Unhealthy);
    assert_eq!(restored.consecutive_failures, 1);
    assert_eq!(restored.last_failure, Some(HealthFailure::Timeout));
    assert_eq!(restored.last_transition_at_unix_ms, Some(base));
}

#[test]
fn a_server_that_moved_to_another_address_starts_over() {
    let old = pool([3000, 3001], false);
    old.record_health(&old.handle(0), true, None, Some(now_unix_ms()), 1, 1);
    let new = pool([4000, 3001], false);

    new.restore_health(&[old.health_carry()]);

    let restored = first_state(&new);
    assert_eq!(restored.state, EndpointHealthState::Unknown);
    assert_eq!(restored.last_checked_at_unix_ms, None);
}

#[test]
fn a_pool_without_health_checks_takes_no_health_state() {
    let old = pool([3000, 3001], false);
    old.record_health(&old.handle(0), true, None, Some(now_unix_ms()), 1, 1);
    let new = RoundRobinPool::new_named_servers_with_policy(
        "carried".into(),
        [
            runtime_server("first", 3000, None),
            runtime_server("second", 3001, None),
        ],
        UpstreamAlgorithm::First,
        None,
        None,
        policy(false),
    )
    .expect("pool without checks");

    new.restore_health(&[old.health_carry()]);

    assert_eq!(first_state(&new).state, EndpointHealthState::Unchecked);
}

#[test]
fn an_open_breaker_stays_open_in_the_replacement_pool() {
    let old = pool([3000, 3001], true);
    old.record_health(&old.handle(0), true, None, Some(now_unix_ms()), 1, 1);
    old.record_health(&old.handle(1), true, None, Some(now_unix_ms()), 1, 1);
    let observation = old.select().expect("lease").observation();
    for _ in 0..4 {
        observation.record_passive_success();
        old.record_passive_failure(0, HealthFailure::Timeout);
    }
    assert_eq!(first_server(&old).as_deref(), Some("second"));
    let new = pool([3000, 3001], true);
    new.record_health(&new.handle(0), true, None, Some(now_unix_ms()), 1, 1);
    new.record_health(&new.handle(1), true, None, Some(now_unix_ms()), 1, 1);
    assert_eq!(first_server(&new).as_deref(), Some("first"));

    new.restore_health(&[old.health_carry()]);

    assert_eq!(first_server(&new).as_deref(), Some("second"));
}

#[test]
fn carried_state_survives_the_control_channel_encoding() {
    let old = pool([3000, 3001], true);
    let base = now_unix_ms();
    old.record_health(&old.handle(0), true, None, Some(base), 1, 1);
    old.record_passive_failure_at(1, HealthFailure::ConnectFailed, base);
    old.record_passive_failure_at(1, HealthFailure::ConnectFailed, base + 1);
    let carried = vec![old.health_carry()];

    let json = serde_json::to_vec(&carried).expect("encoded");
    let decoded: Vec<PoolHealthCarry> = serde_json::from_slice(&json).expect("decoded");

    assert_eq!(decoded, carried);
}

#[test]
fn a_pool_with_more_servers_than_the_room_carries_the_unusual_ones_first() {
    let servers = (0..40)
        .map(|index| ServerHealthCarry {
            server: format!("server-{index:02}"),
            identity: index,
            state: if index == 39 {
                EndpointHealthState::Unhealthy
            } else {
                EndpointHealthState::Healthy
            },
            checked_at: std::num::NonZeroU64::new(1_700_000_000_000),
            transitioned_at: None,
            successes: None,
            failures: None,
            last_failure: None,
            passive: None,
            breaker: None,
        })
        .collect::<Vec<_>>();
    let carried = vec![PoolHealthCarry {
        pool: "wide".into(),
        servers,
    }];
    let one = serde_json::to_vec(&carried[0].servers[0])
        .expect("encoded")
        .len()
        + 1;

    let (kept, left_out) = fit_health_carry(carried, 32 + 4 + one * 10 + 8);

    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].servers.len(), 10);
    assert_eq!(left_out, 30);
    assert_eq!(kept[0].servers[9].server, "server-39");
    assert_eq!(kept[0].servers[0].server, "server-00");
}
