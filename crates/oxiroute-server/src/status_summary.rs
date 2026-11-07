//! The read-only status page JSON: one place decides how bad things are.

use serde::Serialize;

use crate::{
    ApiResponse, GenerationManager, GenerationStatus, PoolHealthSnapshot, RuntimeMetrics,
    RuntimeSnapshot,
    inflight::{InflightTotals, PoolInflight, registry},
};

/// A pool whose oldest request has waited longer than this is queueing for real, not just briefly.
const QUEUE_WARNING_AFTER_MS: u64 = 5_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Severity {
    Ok,
    Warning,
    Critical,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusSummary {
    state: Severity,
    generation: GenerationSummary,
    pools: Vec<PoolSummary>,
    overdue_requests: u64,
    stuck_requests: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationSummary {
    age_seconds: u64,
    degraded: bool,
    last_rejection_at_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PoolSummary {
    name: String,
    state: Severity,
    available: usize,
    total: usize,
    ejected: usize,
    fail_open: bool,
    queued: u64,
    oldest_queue_wait_ms: u64,
    inflight: u64,
    oldest_inflight_ms: u64,
}

impl PoolSummary {
    fn project(pool: &PoolHealthSnapshot, live: PoolInflight) -> Self {
        let state = if pool.available_endpoints == 0 {
            Severity::Critical
        } else if pool.available_endpoints < pool.total_endpoints
            || (pool.queued > 0 && pool.oldest_queue_wait_ms > QUEUE_WARNING_AFTER_MS)
        {
            Severity::Warning
        } else {
            Severity::Ok
        };
        Self {
            name: pool.name.clone(),
            state,
            available: pool.available_endpoints,
            total: pool.total_endpoints,
            ejected: pool
                .endpoints
                .iter()
                .filter(|server| server.passive_ejected)
                .count(),
            fail_open: pool.available_endpoints == 0 && pool.fail_open_selections > 0,
            queued: pool.queued,
            oldest_queue_wait_ms: pool.oldest_queue_wait_ms,
            inflight: live.requests,
            oldest_inflight_ms: live.oldest_age_ms,
        }
    }
}

/// `GET /status.json`: public like `/ready`, so it carries no names beyond pools and no secrets.
pub(crate) fn status_json_response(
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    let Ok(runtime) = metrics.snapshot() else {
        return ApiResponse::error(
            503,
            "status_unavailable",
            "runtime status could not be sampled",
        );
    };
    let generation = generations.map(GenerationManager::status);
    let summary = StatusSummary::project(&runtime, generation.as_ref(), &registry().totals());
    ApiResponse::json(
        200,
        &serde_json::to_value(summary).expect("status summary serializes"),
    )
}

impl StatusSummary {
    pub(crate) fn project(
        runtime: &RuntimeSnapshot,
        generation: Option<&GenerationStatus>,
        inflight: &InflightTotals,
    ) -> Self {
        let pools = runtime
            .upstream_pools
            .iter()
            .map(|pool| {
                let live = inflight.pools.get(&pool.name).copied().unwrap_or_default();
                PoolSummary::project(pool, live)
            })
            .collect::<Vec<_>>();
        let degraded = generation.is_none_or(|status| status.degraded);
        let stuck_requests = inflight.stuck.values().sum();
        let state = pools
            .iter()
            .map(|pool| pool.state)
            .chain((inflight.overdue > 0).then_some(Severity::Critical))
            .chain((degraded || stuck_requests > 0).then_some(Severity::Warning))
            .max()
            .unwrap_or(Severity::Ok);
        Self {
            state,
            generation: GenerationSummary {
                age_seconds: runtime.generation_age_ms / 1_000,
                degraded,
                last_rejection_at_unix_ms: generation
                    .and_then(|status| status.last_rejection.as_ref())
                    .map(|rejection| rejection.at_unix_ms),
            },
            pools,
            overdue_requests: inflight.overdue,
            stuck_requests,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::RuntimeMetrics;

    fn pool(name: &str, available: usize, total: usize) -> PoolHealthSnapshot {
        PoolHealthSnapshot {
            name: name.into(),
            algorithm: "round_robin",
            available_endpoints: available,
            total_endpoints: total,
            unavailable_selections: 0,
            fail_open_selections: 0,
            queued: 0,
            oldest_queue_wait_ms: 0,
            queued_total: 0,
            queue_timeouts: 0,
            queue_cancellations: 0,
            endpoints: Vec::new(),
        }
    }

    fn summary(pools: Vec<PoolHealthSnapshot>, inflight: &InflightTotals) -> serde_json::Value {
        let mut runtime = RuntimeMetrics::new().snapshot().expect("snapshot");
        runtime.upstream_pools = pools;
        let healthy = generation_status();
        serde_json::to_value(StatusSummary::project(&runtime, Some(&healthy), inflight))
            .expect("summary serializes")
    }

    fn generation_status() -> GenerationStatus {
        crate::GenerationManager::new().status()
    }

    #[test]
    fn a_pool_with_every_server_available_is_ok() {
        let body = summary(vec![pool("farm", 4, 4)], &InflightTotals::default());
        assert_eq!(body["pools"][0]["state"], "ok");
        assert_eq!(body["overdueRequests"], 0);
    }

    #[test]
    fn a_missing_server_warns_and_an_empty_pool_is_critical() {
        let mut inflight = InflightTotals::default();
        inflight.pools.insert(
            "farm".into(),
            PoolInflight {
                requests: 3,
                oldest_age_ms: 91_000,
            },
        );
        let body = summary(vec![pool("farm", 3, 4), pool("web", 0, 2)], &inflight);
        assert_eq!(body["pools"][0]["state"], "warning");
        assert_eq!(body["pools"][0]["inflight"], 3);
        assert_eq!(body["pools"][0]["oldestInflightMs"], 91_000);
        assert_eq!(body["pools"][1]["state"], "critical");
        assert_eq!(body["state"], "critical");
    }

    #[test]
    fn a_long_queue_warns_but_a_short_one_does_not() {
        let mut short = pool("farm", 4, 4);
        short.queued = 2;
        short.oldest_queue_wait_ms = 400;
        let mut long = pool("gpu", 4, 4);
        long.queued = 2;
        long.oldest_queue_wait_ms = 9_000;
        let body = summary(vec![short, long], &InflightTotals::default());
        assert_eq!(body["pools"][0]["state"], "ok");
        assert_eq!(body["pools"][1]["state"], "warning");
    }

    #[test]
    fn an_overdue_request_is_critical_and_a_stuck_one_warns() {
        let overdue = InflightTotals {
            overdue: 1,
            ..InflightTotals::default()
        };
        assert_eq!(
            summary(vec![pool("farm", 4, 4)], &overdue)["state"],
            "critical"
        );
        let stuck = InflightTotals {
            stuck: BTreeMap::from([(("farm".to_owned(), "queued"), 2)]),
            ..InflightTotals::default()
        };
        let body = summary(vec![pool("farm", 4, 4)], &stuck);
        assert_eq!(
            (body["state"].as_str(), body["stuckRequests"].as_u64()),
            (Some("warning"), Some(2))
        );
    }
}
