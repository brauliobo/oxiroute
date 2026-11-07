//! The read-only status page JSON: one place assembles it, `status_check` decides how bad it is.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{
    ApiResponse, GenerationManager, GenerationStatus, PoolHealthSnapshot, RuntimeMetrics,
    RuntimeSnapshot,
    inflight::{InflightTotals, PoolInflight, registry},
    registry::service::{RegistrationStatus, RegistrationView, RegistryService, RegistryStatus},
    rtmp_api::dto::ReadinessResponse,
    status_check::Thresholds,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Severity {
    Ok,
    Warning,
    Critical,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusSummary {
    pub(crate) state: Severity,
    pub(crate) ready: bool,
    pub(crate) generation: GenerationSummary,
    pub(crate) pools: Vec<PoolSummary>,
    pub(crate) overdue_requests: u64,
    pub(crate) stuck_requests: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) registry: Option<RegistrySummary>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerationSummary {
    pub(crate) id: u64,
    pub(crate) age_seconds: u64,
    pub(crate) degraded: bool,
    pub(crate) retired: usize,
    pub(crate) retired_oldest_age_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_rejection: Option<RejectionSummary>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RejectionSummary {
    pub(crate) at_unix_ms: u64,
    pub(crate) age_seconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PoolSummary {
    pub(crate) name: String,
    pub(crate) state: Severity,
    pub(crate) available: usize,
    pub(crate) total: usize,
    pub(crate) ejected: usize,
    pub(crate) fail_open: bool,
    pub(crate) queued: u64,
    pub(crate) oldest_queue_wait_ms: u64,
    pub(crate) inflight: u64,
    pub(crate) oldest_inflight_ms: u64,
}

/// What the registry says about its leases; absent when the process has no `registry` block.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegistrySummary {
    pub(crate) degraded: bool,
    pub(crate) leases: usize,
    /// Leases whose holder missed a heartbeat: less than a third of the TTL is left.
    pub(crate) late_leases: usize,
    pub(crate) shadowed: usize,
    pub(crate) orphaned: usize,
}

impl RegistrySummary {
    fn project(status: &RegistryStatus, views: &[RegistrationView]) -> Self {
        let count =
            |keep: &dyn Fn(&RegistrationView) -> bool| views.iter().filter(|v| keep(v)).count();
        Self {
            degraded: status.degraded.is_some(),
            leases: count(&|view| view.expires_in_ms.is_some()),
            late_leases: count(&|view| {
                view.expires_in_ms
                    .is_some_and(|left| left.saturating_mul(3) < view.record.ttl_ms)
            }),
            shadowed: count(&|view| view.status == RegistrationStatus::Shadowed),
            orphaned: count(&|view| view.status == RegistrationStatus::Orphaned),
        }
    }
}

impl PoolSummary {
    fn project(pool: &PoolHealthSnapshot, live: PoolInflight) -> Self {
        let mut summary = Self {
            name: pool.name.clone(),
            state: Severity::Ok,
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
        };
        summary.state = StatusSummary::pool_severity(&summary);
        summary
    }
}

/// `GET /status.json`: public like `/ready`, so it carries no names beyond pools and no secrets.
pub(crate) fn status_json_response(
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    let registry = generations.and_then(GenerationManager::registry);
    match status_document(metrics, generations, registry.as_deref()) {
        Some(document) => ApiResponse::json(200, &document),
        None => ApiResponse::error(
            503,
            "status_unavailable",
            "runtime status could not be sampled",
        ),
    }
}

/// The status page as JSON; `None` when the runtime cannot be sampled.
pub(crate) fn status_document(
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
    registry_service: Option<&RegistryService>,
) -> Option<serde_json::Value> {
    let runtime = metrics.snapshot().ok()?;
    let generation = generations.map(GenerationManager::status);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let mut summary = StatusSummary::project(
        &runtime,
        generation.as_ref(),
        &registry().totals(),
        Moment {
            ready: ReadinessResponse::project(&runtime, generation.clone()).ready(),
            unix_ms: u64::try_from(now).unwrap_or(u64::MAX),
        },
    );
    summary.registry = registry_service.and_then(|service| {
        Some(RegistrySummary::project(
            &service.status(),
            &service.list(None).ok()?,
        ))
    });
    summary.state = summary.judge(&Thresholds::default()).severity;
    Some(serde_json::to_value(summary).expect("status summary serializes"))
}

/// What the sampler knows besides the runtime: readiness and the wall clock.
#[derive(Clone, Copy)]
pub(crate) struct Moment {
    pub(crate) ready: bool,
    pub(crate) unix_ms: u64,
}

impl StatusSummary {
    pub(crate) fn project(
        runtime: &RuntimeSnapshot,
        generation: Option<&GenerationStatus>,
        inflight: &InflightTotals,
        moment: Moment,
    ) -> Self {
        let pools = runtime
            .upstream_pools
            .iter()
            .map(|pool| {
                let live = inflight.pools.get(&pool.name).copied().unwrap_or_default();
                PoolSummary::project(pool, live)
            })
            .collect::<Vec<_>>();
        let mut summary = Self {
            state: Severity::Ok,
            ready: moment.ready,
            generation: GenerationSummary::project(runtime, generation, moment.unix_ms),
            pools,
            overdue_requests: inflight.overdue,
            stuck_requests: inflight.stuck.values().sum(),
            registry: None,
        };
        summary.state = summary.judge(&Thresholds::default()).severity;
        summary
    }
}

impl GenerationSummary {
    fn project(
        runtime: &RuntimeSnapshot,
        status: Option<&GenerationStatus>,
        now_unix_ms: u64,
    ) -> Self {
        let rejection = status.and_then(|status| status.last_rejection.as_ref());
        Self {
            id: status.map_or(0, |status| status.generation),
            age_seconds: runtime.generation_age_ms / 1_000,
            degraded: status.is_none_or(|status| status.degraded),
            retired: status.map_or(0, |status| status.retired_generations.len()),
            retired_oldest_age_seconds: status
                .and_then(|status| {
                    status
                        .retired_generations
                        .iter()
                        .map(|g| g.age_seconds)
                        .max()
                })
                .unwrap_or(0),
            last_rejection: rejection.map(|rejection| RejectionSummary {
                at_unix_ms: rejection.at_unix_ms,
                age_seconds: now_unix_ms.saturating_sub(rejection.at_unix_ms) / 1_000,
            }),
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
            queued_by_class: Vec::new(),
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
        serde_json::to_value(StatusSummary::project(
            &runtime,
            Some(&healthy),
            inflight,
            Moment {
                ready: true,
                unix_ms: 0,
            },
        ))
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

    fn view(
        name: &str,
        ttl_ms: u64,
        left: Option<u64>,
        status: RegistrationStatus,
    ) -> RegistrationView {
        RegistrationView {
            record: crate::registry::record::Registration {
                pool: "farm".into(),
                name: name.into(),
                address: "192.0.2.1:9000".parse().expect("address"),
                weight: 1,
                priority: 100,
                max_connections: None,
                labels: BTreeMap::new(),
                metadata: serde_json::Value::Null,
                ttl_ms,
                announced_state: crate::registry::record::AnnouncedState::Ready,
                operator_state: crate::AdministrativeState::Ready,
                registered_by: "test".into(),
            },
            expires_in_ms: left,
            status,
        }
    }

    #[test]
    fn leases_a_third_from_expiry_count_as_late_and_unusable_records_are_counted() {
        let status = RegistryStatus {
            revision: 4,
            writer: true,
            degraded: None,
            records: 4,
        };
        let views = [
            view("fresh", 15_000, Some(14_000), RegistrationStatus::Active),
            view("late", 15_000, Some(4_000), RegistrationStatus::Active),
            view(
                "shadowed",
                15_000,
                Some(9_000),
                RegistrationStatus::Shadowed,
            ),
            view("orphan", 15_000, None, RegistrationStatus::Orphaned),
        ];
        let body = serde_json::to_value(RegistrySummary::project(&status, &views)).expect("json");
        assert_eq!(
            body,
            serde_json::json!({
                "degraded": false, "leases": 3, "lateLeases": 1, "shadowed": 1, "orphaned": 1,
            })
        );
    }
}
