use oxiroute_config::ConfigDraft;

use super::{
    super::model::{
        CanaryStats, CheckStats, ClassCount, InflightStats, PassiveStats, PoolStats, PoolStatus,
        QueueStats, ServerStats, ServerStatus,
    },
    traffic::{Hist, Totals},
};
use crate::{
    AdministrativeState as Administrative, EndpointHealthSnapshot, EndpointHealthState,
    HealthOverride as Override, PoolHealthSnapshot, RuntimeSnapshot,
    http::inflight::InflightTotals,
    observability::upstream_stats::{QUEUE_WAIT_BUCKETS_MS, upstream_stats},
    routing::CanarySnapshot,
};

pub(super) fn pools(
    runtime: &RuntimeSnapshot,
    config: &ConfigDraft,
    inflight: &InflightTotals,
) -> Vec<PoolStats> {
    runtime
        .upstream_pools
        .iter()
        .map(|pool| pool_stats(pool, runtime.sampled_at_unix_ms, config, inflight))
        .collect()
}

fn pool_stats(
    pool: &PoolHealthSnapshot,
    now_unix_ms: u64,
    config: &ConfigDraft,
    inflight: &InflightTotals,
) -> PoolStats {
    let stats = upstream_stats();
    let mut total = Totals::default();
    let servers = pool
        .endpoints
        .iter()
        .map(|endpoint| {
            let raw = Totals::of(
                &stats.server(&pool.name, &endpoint.name).snapshot(),
                endpoint.active_connections,
            );
            total.add(&raw);
            server_stats(endpoint, &raw, now_unix_ms)
        })
        .collect();
    let live = inflight.pools.get(&pool.name).copied().unwrap_or_default();
    PoolStats {
        name: pool.name.clone(),
        algorithm: pool.algorithm.to_owned(),
        status: pool_status(pool),
        available_servers: pool.available_endpoints as u64,
        total_servers: pool.total_endpoints as u64,
        unavailable_selections: pool.unavailable_selections.into(),
        fail_open_selections: pool.fail_open_selections.into(),
        queue: QueueStats {
            current: pool.queued,
            by_class: (!pool.queued_by_class.is_empty()).then(|| {
                pool.queued_by_class
                    .iter()
                    .map(|(class, count)| ClassCount {
                        class: class.clone(),
                        count: *count,
                    })
                    .collect()
            }),
            oldest_wait_ms: pool.oldest_queue_wait_ms,
            total: pool.queued_total.into(),
            timeouts: pool.queue_timeouts.into(),
            cancellations: pool.queue_cancellations.into(),
            timeout_ms: config
                .upstream_pools
                .iter()
                .find(|configured| configured.name == pool.name)
                .and_then(|configured| configured.queue_timeout_ms),
            wait: Hist::from(&stats.pool(&pool.name).queue_wait())
                .to_wire(QUEUE_WAIT_BUCKETS_MS.len()),
        },
        inflight: InflightStats {
            requests: live.requests,
            oldest_age_ms: live.oldest_age_ms,
        },
        traffic: total.traffic(),
        latency: total.latency(),
        canary: pool.canary.as_ref().map(canary),
        servers,
    }
}

fn pool_status(pool: &PoolHealthSnapshot) -> PoolStatus {
    match (pool.total_endpoints, pool.available_endpoints) {
        (0, _) => PoolStatus::Empty,
        (_, 0) => PoolStatus::Down,
        (total, available) if available < total => PoolStatus::Degraded,
        _ => PoolStatus::Up,
    }
}

fn canary(canary: &CanarySnapshot) -> CanaryStats {
    CanaryStats {
        result: canary.result.map(Into::into),
        reason: canary.reason.map(Into::into),
        server: canary.server.clone(),
        duration_ms: canary.duration_ms,
        last_attempt_unix_ms: canary.last_attempt_unix_ms,
        last_success_unix_ms: canary.last_success_unix_ms,
        consecutive_failures: canary.consecutive_failures,
        interval_ms: canary.interval_ms,
    }
}

/// What a person reads first. Administrative state wins over ejection, which wins over health.
pub(crate) fn server_status(endpoint: &EndpointHealthSnapshot) -> ServerStatus {
    match (
        endpoint.administrative_state,
        endpoint.health_override,
        endpoint.state,
    ) {
        (Administrative::Maintenance, ..) => ServerStatus::Maint,
        (Administrative::Drain, ..) => ServerStatus::Drain,
        _ if endpoint.passive_ejected => ServerStatus::Ejected,
        (_, Override::Down, _) | (_, Override::Auto, EndpointHealthState::Unhealthy) => {
            ServerStatus::Down
        }
        (_, Override::Up, _) | (_, Override::Auto, EndpointHealthState::Healthy) => {
            ServerStatus::Up
        }
        (_, Override::Auto, EndpointHealthState::Unchecked) => ServerStatus::Unchecked,
        (_, Override::Auto, EndpointHealthState::Unknown) => ServerStatus::Unknown,
    }
}

fn server_stats(endpoint: &EndpointHealthSnapshot, raw: &Totals, now_unix_ms: u64) -> ServerStats {
    let status = server_status(endpoint);
    ServerStats {
        name: endpoint.name.clone(),
        address: endpoint.address.to_string(),
        origin: endpoint.origin.into(),
        status,
        available: matches!(status, ServerStatus::Up | ServerStatus::Unchecked),
        administrative_state: endpoint.administrative_state.into(),
        health_override: endpoint.health_override.into(),
        health: endpoint.state.into(),
        weight: endpoint.weight,
        configured_weight: endpoint.configured_weight,
        max_connections: endpoint.max_connections,
        configured_max_connections: endpoint.configured_max_connections,
        last_transition_at_unix_ms: endpoint.last_transition_at_unix_ms,
        downtime_ms: endpoint
            .last_transition_at_unix_ms
            .filter(|_| endpoint.state == EndpointHealthState::Unhealthy)
            .map(|at| now_unix_ms.saturating_sub(at)),
        check: CheckStats {
            enabled: endpoint.checks_enabled,
            running: endpoint.checks_running,
            successful: endpoint.successful_checks.into(),
            failed: endpoint.failed_checks.into(),
            consecutive_successes: endpoint.consecutive_successes,
            consecutive_failures: endpoint.consecutive_failures,
            last_checked_at_unix_ms: endpoint.last_checked_at_unix_ms,
            last_failure: endpoint.last_failure.map(Into::into),
        },
        passive: PassiveStats {
            ejected: endpoint.passive_ejected,
            failure_count: endpoint.passive_failure_count.into(),
            consecutive_failures: endpoint.passive_consecutive_failures,
            ejection_count: endpoint.passive_ejection_count.into(),
            ejection_reason: endpoint.passive_ejection_reason.map(Into::into),
            ejected_at_unix_ms: endpoint.passive_ejected_at_unix_ms,
            ejection_until_unix_ms: endpoint.passive_ejection_until_unix_ms,
            recovery_count: endpoint.passive_recovery_count.into(),
            last_recovery_at_unix_ms: endpoint.passive_last_recovery_at_unix_ms,
        },
        traffic: raw.traffic(),
        latency: raw.latency(),
    }
}
