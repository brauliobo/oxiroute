//! Builds the page snapshot from the active generation. It only reads state the runtime already
//! keeps: no hot-path counter is added for it.

use std::{collections::BTreeMap, sync::Arc};

use super::model::{
    BucketBounds, GenerationStats, HostStats, ProcessInflight, ProcessStats, RejectionStats,
    RetiredGeneration, SCHEMA_VERSION, StatsSnapshot, WarningStats,
};
use crate::{
    GenerationManager, GenerationStatus, MetricsError, OPERATION_LATENCY_BUCKETS_MS,
    RuntimeGeneration, RuntimeSnapshot,
    http::inflight::{InflightTotals, registry},
    observability::upstream_stats::QUEUE_WAIT_BUCKETS_MS,
};

pub(super) mod certificates;
mod listeners;
pub(super) mod pools;
mod routes;
mod traffic;

/// The page cannot show anything until a generation is active and the runtime can be sampled.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildError {
    #[error("no generation is active")]
    NoActiveGeneration,
    #[error("runtime metrics could not be sampled")]
    Metrics(#[from] MetricsError),
}

/// Never poll faster than this: it is also how long the page listener keeps a built snapshot.
pub(crate) const MIN_INTERVAL_MS: u64 = 1_000;

pub(crate) fn build_snapshot(
    generations: &GenerationManager,
    refresh_ms: u64,
) -> Result<StatsSnapshot, BuildError> {
    let generation = generations.active().ok_or(BuildError::NoActiveGeneration)?;
    let runtime = generation.metrics().snapshot()?;
    let status = generations.status();
    let inflight = registry().totals();
    let config = generation.config().as_draft();
    Ok(StatsSnapshot {
        schema_version: SCHEMA_VERSION,
        sampled_at_unix_ms: runtime.sampled_at_unix_ms,
        interval_hint_ms: refresh_ms.max(MIN_INTERVAL_MS),
        process: process(&runtime, &inflight),
        generation: generation_stats(&runtime, &status),
        bucket_bounds_ms: BucketBounds {
            operation: OPERATION_LATENCY_BUCKETS_MS.to_vec(),
            queue_wait: QUEUE_WAIT_BUCKETS_MS.to_vec(),
        },
        listeners: listeners::listeners(&runtime, config),
        routes: routes::routes(&runtime, config),
        pools: pools::pools(&runtime, config, &inflight),
        certificates: certificates::list(&runtime),
        certificate_watchers: certificates::watchers(&runtime),
        warnings: warnings(&generation),
    })
}

fn process(runtime: &RuntimeSnapshot, inflight: &InflightTotals) -> ProcessStats {
    let process = &runtime.process;
    let host = &runtime.host;
    ProcessStats {
        version: crate::cli::BUILD_VERSION.to_owned(),
        uptime_ms: runtime.uptime_ms,
        state: process.status.state.into(),
        state_reason: process.status.reason.map(str::to_owned),
        administrative_state: process.administrative_state.into(),
        cpu_percent: process.cpu_percent,
        resident_memory_bytes: process.resident_memory_bytes,
        virtual_memory_bytes: process.virtual_memory_bytes,
        thread_count: process.thread_count,
        open_file_descriptors: process.open_file_descriptors,
        active_connections: process.active_connections,
        max_connections: process.max_connections,
        accepted_connections: runtime.traffic.accepted_connections.into(),
        rejected_connections: process.rejected_connections.into(),
        bytes_received: runtime.traffic.bytes_received.into(),
        bytes_sent: runtime.traffic.bytes_sent.into(),
        retry_attempts: process.retry_attempts.into(),
        inflight: ProcessInflight {
            upstream_requests: inflight.pools.values().map(|pool| pool.requests).sum(),
            oldest_upstream_age_ms: inflight
                .pools
                .values()
                .map(|pool| pool.oldest_age_ms)
                .max()
                .unwrap_or(0),
            overdue: inflight.overdue,
            stuck: inflight.stuck.values().sum(),
        },
        host: HostStats {
            state: host.status.state.into(),
            load_average_1m: host.load_average_1m,
            load_average_5m: host.load_average_5m,
            load_average_15m: host.load_average_15m,
            total_memory_bytes: host.total_memory_bytes,
            available_memory_bytes: host.available_memory_bytes,
        },
    }
}

fn generation_stats(runtime: &RuntimeSnapshot, status: &GenerationStatus) -> GenerationStats {
    GenerationStats {
        number: status.generation,
        age_ms: runtime.generation_age_ms,
        accepting: status.active_accepting,
        degraded: status.degraded,
        active_revision: status
            .active_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
        candidate_revision: status
            .candidate_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
        previous_revision: status
            .previous_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
        disk_revision: status
            .disk_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
        quarantined_revision: status
            .quarantined_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
        last_failure: status.last_failure.map(str::to_owned),
        last_rejection: status
            .last_rejection
            .as_ref()
            .map(|rejection| RejectionStats {
                at_unix_ms: rejection.at_unix_ms,
                revision: rejection.revision.clone(),
                diagnostics: rejection
                    .diagnostics
                    .iter()
                    .map(|diagnostic| super::model::DiagnosticStats {
                        code: diagnostic.code.clone(),
                        stage: diagnostic.stage.clone(),
                        message: diagnostic.message.clone(),
                    })
                    .collect(),
            }),
        prepares: status.prepares.into(),
        activations: status.activations.into(),
        config_rejections: status.config_rejections.into(),
        failures: status.failures.into(),
        rollbacks: status.rollbacks.into(),
        max_retired_generations: status.max_retired_generations as u64,
        retired: status
            .retired_generations
            .iter()
            .map(|retired| RetiredGeneration {
                revision: retired.revision.as_str().to_owned(),
                state: retired.state.into(),
                age_seconds: retired.age_seconds,
                deadline_remaining_seconds: retired.deadline_remaining_seconds,
                in_flight: retired.in_flight,
                in_flight_by_kind: retired
                    .in_flight_by_kind
                    .iter()
                    .map(|(kind, count)| ((*kind).to_owned(), *count))
                    .collect::<BTreeMap<_, _>>(),
            })
            .collect(),
    }
}

fn warnings(generation: &Arc<RuntimeGeneration>) -> Vec<WarningStats> {
    generation
        .config()
        .warnings()
        .into_iter()
        .map(|warning| WarningStats {
            code: warning.code.to_owned(),
            message: warning.message,
        })
        .collect()
}
