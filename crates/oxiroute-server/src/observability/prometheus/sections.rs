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
    server: &crate::EndpointHealthSnapshot,
    histograms: bool,
) -> Result<(), PrometheusError> {
    let stats = crate::observability::upstream_stats::upstream_stats()
        .server(pool, &server.name)
        .snapshot();
    let identity = [("pool", pool), ("server", server.name.as_str())];
    let with = |label: (&'static str, &'static str)| [identity[0], identity[1], label];
    for (class, count) in crate::observability::upstream_stats::RESPONSE_CLASSES
        .iter()
        .zip(stats.responses)
    {
        labels(
            output,
            "oxiroute_server_responses_total",
            &with(("class", *class)),
            count,
        )?;
    }
    for (outcome, count) in crate::observability::upstream_stats::AttemptOutcome::ALL
        .iter()
        .zip(stats.attempts)
    {
        let outcome = outcome.as_str();
        labels(
            output,
            "oxiroute_server_attempts_total",
            &with(("outcome", outcome)),
            count,
        )?;
    }
    for (result, count) in crate::observability::upstream_stats::EvacuationResult::ALL
        .iter()
        .zip(stats.evacuated)
    {
        labels(
            output,
            "oxiroute_evacuated_requests_total",
            &with(("result", result.as_str())),
            count,
        )?;
    }
    labels(
        output,
        "oxiroute_server_bytes_sent_total",
        &identity,
        stats.bytes_sent,
    )?;
    labels(
        output,
        "oxiroute_server_bytes_received_total",
        &identity,
        stats.bytes_received,
    )?;
    if let Some(limit) = server.max_connections {
        labels(output, "oxiroute_server_max_connections", &identity, limit)?;
    }
    if histograms {
        let request = "oxiroute_server_request_duration_milliseconds";
        render_histogram(output, request, &identity, &stats.request_duration)?;
        let header = "oxiroute_server_header_duration_milliseconds";
        render_histogram(output, header, &identity, &stats.header_duration)?;
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
