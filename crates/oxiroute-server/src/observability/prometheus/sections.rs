#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::cast_precision_loss)]
pub(super) fn render_inflight(output: &mut String) -> Result<(), PrometheusError> {
    let inflight = crate::http::inflight::registry().totals();
    for (pool, live) in &inflight.pools {
        let pool = [("pool", pool.as_str())];
        labels(
            output,
            "oxiroute_http_inflight_requests",
            &pool,
            live.requests,
        )?;
        let oldest = live.oldest_age_ms as f64 / 1_000.0;
        labels(
            output,
            "oxiroute_http_inflight_oldest_age_seconds",
            &pool,
            oldest,
        )?;
    }
    sample(output, "oxiroute_http_inflight_overdue", inflight.overdue)?;
    for ((pool, phase), count) in &inflight.stuck {
        let series = [("pool", pool.as_str()), ("phase", *phase)];
        labels(
            output,
            "oxiroute_http_stuck_requests_total",
            &series,
            *count,
        )?;
    }
    Ok(())
}

pub(super) fn render_server_traffic(
    output: &mut String,
    pool: &str,
    identity: &str,
    server: &crate::EndpointHealthSnapshot,
    histograms: bool,
) -> Result<(), PrometheusError> {
    use crate::observability::upstream_stats::{
        AttemptOutcome, EvacuationResult, RESPONSE_CLASSES, upstream_stats,
    };
    let stats = upstream_stats().server(pool, &server.name).snapshot();
    let series = |output: &mut String, name, extra, value: u64| {
        with_prefix(output, name, identity, extra, value);
    };
    for (class, count) in RESPONSE_CLASSES.iter().zip(stats.responses) {
        series(
            output,
            "oxiroute_server_responses_total",
            Some(("class", class)),
            count,
        );
    }
    for (outcome, count) in AttemptOutcome::ALL.iter().zip(stats.attempts) {
        let extra = Some(("outcome", outcome.as_str()));
        series(output, "oxiroute_server_attempts_total", extra, count);
    }
    for (result, count) in EvacuationResult::ALL.iter().zip(stats.evacuated) {
        let extra = Some(("result", result.as_str()));
        series(output, "oxiroute_evacuated_requests_total", extra, count);
    }
    series(
        output,
        "oxiroute_server_bytes_sent_total",
        None,
        stats.bytes_sent,
    );
    series(
        output,
        "oxiroute_server_bytes_received_total",
        None,
        stats.bytes_received,
    );
    if let Some(limit) = server.max_connections {
        series(output, "oxiroute_server_max_connections", None, limit);
    }
    if histograms {
        let identity = [("pool", pool), ("server", server.name.as_str())];
        let request = "oxiroute_server_request_duration_milliseconds";
        render_histogram(output, request, &identity, &stats.request_duration)?;
        let header = "oxiroute_server_header_duration_milliseconds";
        render_histogram(output, header, &identity, &stats.header_duration)?;
    }
    Ok(())
}

/// The connection scheduling mode of the serving generation and, with `aged_per_worker`, what its queues hold.
#[allow(clippy::cast_precision_loss)]
pub(super) fn render_connection_scheduling(
    output: &mut String,
    config: Option<&oxiroute_config::ConfigDraft>,
) -> Result<(), PrometheusError> {
    let Some(config) = config.filter(|config| config.aged_per_worker()) else {
        return Ok(labels(
            output,
            "oxiroute_connection_scheduling_info",
            &[("mode", "work_stealing")],
            1,
        )?);
    };
    let age = config
        .connection_age_ms
        .map_or_else(|| "adaptive".to_owned(), |age| age.to_string());
    let queues = oxiroute_aged_executor::AgedExecutor::process_stats();
    labels(
        output,
        "oxiroute_connection_scheduling_info",
        &[("mode", "aged_per_worker"), ("age_ms", &age)],
        1,
    )?;
    sample(output, "oxiroute_aged_queue_tasks", queues.tasks)?;
    sample(output, "oxiroute_aged_queue_ready", queues.ready)?;
    sample(
        output,
        "oxiroute_aged_queue_oldest_ready_seconds",
        queues.oldest_ready.as_secs_f64(),
    )?;
    sample(
        output,
        "oxiroute_aged_queue_age_seconds",
        queues.age.as_secs_f64(),
    )?;
    sample(output, "oxiroute_aged_queue_polls_total", queues.polls)?;
    sample(
        output,
        "oxiroute_aged_queue_aged_polls_total",
        queues.aged_polls,
    )?;
    Ok(())
}

/// The worker scaling mode and, with `dynamic`, how many workers each listener runtime keeps active.
pub(super) fn render_worker_scaling(
    output: &mut String,
    config: Option<&oxiroute_config::ConfigDraft>,
) -> Result<(), PrometheusError> {
    let dynamic = config.is_some_and(oxiroute_config::ConfigDraft::dynamic_worker_scaling);
    let mode = if dynamic { "dynamic" } else { "static" };
    labels(output, "oxiroute_worker_scaling_info", &[("mode", mode)], 1)?;
    if !dynamic {
        return Ok(());
    }
    for (runtime, stats) in oxiroute_worker_scaling::process_stats() {
        let identity = [("runtime", runtime.as_str())];
        labels(
            output,
            "oxiroute_worker_scaling_workers",
            &identity,
            stats.workers,
        )?;
        labels(
            output,
            "oxiroute_worker_scaling_active_workers",
            &identity,
            stats.active,
        )?;
        labels(
            output,
            "oxiroute_worker_scaling_relocations_total",
            &identity,
            stats.relocations,
        )?;
    }
    Ok(())
}

pub(super) fn render_retirement(
    output: &mut String,
    generations: &crate::GenerationManager,
    generation: &crate::GenerationStatus,
) -> Result<(), PrometheusError> {
    let retired = &generation.retired_generations;
    sample(output, "oxiroute_generation_retired", retired.len())?;
    for entry in retired {
        let revision = revision_label(entry.revision.as_str());
        metric(
            output,
            "oxiroute_generation_retired_age_seconds",
            "revision",
            revision,
            entry.age_seconds,
        )?;
        let labels_for = [("revision", revision), ("role", "retired")];
        labels(
            output,
            "oxiroute_generation_in_flight",
            &labels_for,
            entry.in_flight,
        )?;
    }
    if let Some(active) = generations.active() {
        let revision = revision_label(active.revision().candidate.as_str());
        labels(
            output,
            "oxiroute_generation_in_flight",
            &[("revision", revision), ("role", "active")],
            active.in_flight(),
        )?;
    }
    let counters = generations.retirement_counters();
    metric(
        output,
        "oxiroute_generation_forced_close_total",
        "reason",
        "drain_deadline",
        counters.forced_close_drain_deadline,
    )?;
    metric(
        output,
        "oxiroute_generation_forced_close_total",
        "reason",
        "shutdown",
        counters.forced_close_shutdown,
    )?;
    sample(
        output,
        "oxiroute_generation_retired_cap_deferrals_total",
        counters.cap_deferrals,
    )?;
    Ok(())
}

/// Revisions are 64 hex digits; the first twelve identify one in a label.
pub(super) fn revision_label(revision: &str) -> &str {
    &revision[..revision.len().min(12)]
}

pub(super) fn render_canary(
    output: &mut String,
    pool: &str,
    canary: &CanarySnapshot,
) -> Result<(), PrometheusError> {
    let pool_label = [("pool", pool)];
    if let Some(result) = canary.result {
        labels(
            output,
            "oxiroute_canary_success",
            &pool_label,
            u8::from(result.healthy()),
        )?;
        labels(
            output,
            "oxiroute_canary_duration_milliseconds",
            &pool_label,
            canary.duration_ms,
        )?;
    }
    if let Some(at) = canary.last_success_unix_ms {
        labels(
            output,
            "oxiroute_canary_last_success_timestamp_seconds",
            &pool_label,
            at / 1_000,
        )?;
    }
    labels(
        output,
        "oxiroute_canary_consecutive_failures",
        &pool_label,
        canary.consecutive_failures,
    )?;
    for (result, runs) in &canary.runs {
        labels(
            output,
            "oxiroute_canary_runs_total",
            &[("pool", pool), ("result", result)],
            *runs,
        )?;
    }
    Ok(())
}
