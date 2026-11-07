use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use oxiroute_rtmp::{RtmpCatalogSnapshot, RtmpControlHandle};

use crate::{GenerationManager, RuntimeMetrics, routing::CanarySnapshot};

mod format;
mod registry;
use format::{
    component_state, endpoint_state, labels, listener_state, metric, render_histogram,
    render_latency_histogram, render_transport_latency_histogram, sample,
};

#[allow(clippy::cast_precision_loss)]
fn render_inflight(output: &mut String) -> Result<(), PrometheusError> {
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

/// Per-server latency histograms cost about 18 series each, so they stop above this many servers.
const SERVER_HISTOGRAM_LIMIT: usize = 256;

fn render_server_traffic(
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

pub(crate) fn render_prometheus_control(
    metrics: &RuntimeMetrics,
    control: &RtmpControlHandle,
    generations: &GenerationManager,
) -> Result<String, PrometheusError> {
    let rtmp = control.catalog_snapshot();
    render_prometheus_snapshot(
        metrics,
        &rtmp,
        control
            .session_snapshots()
            .into_iter()
            .filter(|client| client.connected)
            .count(),
        generations,
    )
}

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn render_prometheus_snapshot(
    metrics: &RuntimeMetrics,
    rtmp: &RtmpCatalogSnapshot,
    rtmp_clients: usize,
    generations: &GenerationManager,
) -> Result<String, PrometheusError> {
    let runtime = metrics.snapshot()?;
    let auto_push = generations
        .active()
        .map_or_else(oxiroute_rtmp::RtmpAutoPushStatus::default, |generation| {
            generation.rtmp_auto_push_status()
        });
    let generation = generations.status();
    let audit = crate::observability::operational_event::audit_metrics();
    let transport_events = crate::observability::monitoring::transport_event_snapshots();
    let mut output = String::with_capacity(16 * 1024);

    metric(
        &mut output,
        "oxiroute_build_info",
        "version",
        crate::cli::BUILD_VERSION,
        1,
    )?;
    sample(
        &mut output,
        "oxiroute_process_uptime_seconds",
        runtime.uptime_ms as f64 / 1_000.0,
    )?;
    sample(
        &mut output,
        "oxiroute_generation_active_age_seconds",
        runtime.generation_age_ms as f64 / 1_000.0,
    )?;
    labels(
        &mut output,
        "oxiroute_process_sampling_state",
        &[("state", component_state(runtime.process.status.state))],
        1,
    )?;
    labels(
        &mut output,
        "oxiroute_host_sampling_state",
        &[("state", component_state(runtime.host.status.state))],
        1,
    )?;
    sample(
        &mut output,
        "oxiroute_process_active_connections",
        runtime.process.active_connections,
    )?;
    sample(
        &mut output,
        "oxiroute_process_rejected_connections_total",
        runtime.process.rejected_connections,
    )?;
    sample(
        &mut output,
        "oxiroute_upstream_retry_attempts_total",
        runtime.process.retry_attempts,
    )?;
    for rescue in &runtime.process.rescues {
        labels(
            &mut output,
            "oxiroute_http_rescues_total",
            &[
                ("route", rescue.route.as_str()),
                ("status", &rescue.status.to_string()),
            ],
            rescue.count,
        )?;
    }
    for selection in &runtime.process.pool_selections {
        labels(
            &mut output,
            "oxiroute_route_pool_selections_total",
            &[
                ("route", selection.route.as_str()),
                ("pool", selection.pool.as_str()),
            ],
            selection.count,
        )?;
    }
    if let Some(value) = runtime.process.resident_memory_bytes {
        sample(&mut output, "oxiroute_process_resident_memory_bytes", value)?;
    }
    if let Some(value) = runtime.process.virtual_memory_bytes {
        sample(&mut output, "oxiroute_process_virtual_memory_bytes", value)?;
    }
    if let Some(value) = runtime.process.thread_count {
        sample(&mut output, "oxiroute_process_threads", value)?;
    }
    if let Some(value) = runtime.process.open_file_descriptors {
        sample(&mut output, "oxiroute_process_open_file_descriptors", value)?;
    }
    if let Some(cpu) = runtime.process.cpu_percent {
        sample(&mut output, "oxiroute_process_cpu_percent", cpu)?;
    }
    if let Some(value) = runtime.host.load_average_1m {
        sample(&mut output, "oxiroute_host_load_average_1m", value)?;
    }
    if let Some(value) = runtime.host.load_average_5m {
        sample(&mut output, "oxiroute_host_load_average_5m", value)?;
    }
    if let Some(value) = runtime.host.load_average_15m {
        sample(&mut output, "oxiroute_host_load_average_15m", value)?;
    }
    if let Some(value) = runtime.host.total_memory_bytes {
        sample(&mut output, "oxiroute_host_total_memory_bytes", value)?;
    }
    if let Some(value) = runtime.host.available_memory_bytes {
        sample(&mut output, "oxiroute_host_available_memory_bytes", value)?;
    }

    for operation in &runtime.transport_operations {
        for outcome in &operation.outcomes {
            labels(
                &mut output,
                "oxiroute_transport_operations_total",
                &[
                    ("transport", operation.transport.as_str()),
                    ("outcome", outcome.outcome.as_str()),
                ],
                outcome.count,
            )?;
        }
        render_transport_latency_histogram(
            &mut output,
            "oxiroute_transport_operation_duration_milliseconds",
            operation.transport.as_str(),
            &operation.latency,
        )?;
    }
    for operation in transport_events {
        for outcome in operation.outcomes {
            labels(
                &mut output,
                "oxiroute_transport_events_total",
                &[
                    ("transport", operation.transport.as_str()),
                    ("outcome", outcome.outcome.as_str()),
                ],
                outcome.count,
            )?;
        }
    }

    for listener in &runtime.listeners {
        labels(
            &mut output,
            "oxiroute_listener_state",
            &[
                ("listener", listener.name.as_str()),
                ("protocol", listener.protocol.as_str()),
                ("state", listener_state(listener.state)),
            ],
            1,
        )?;
        labels(
            &mut output,
            "oxiroute_listener_active_connections",
            &[("listener", listener.name.as_str())],
            listener.active_connections,
        )?;
        labels(
            &mut output,
            "oxiroute_listener_accepted_connections_total",
            &[("listener", listener.name.as_str())],
            listener.accepted_connections,
        )?;
        labels(
            &mut output,
            "oxiroute_listener_rejected_connections_total",
            &[("listener", listener.name.as_str())],
            listener.rejected_connections,
        )?;
        labels(
            &mut output,
            "oxiroute_listener_bytes_received_total",
            &[("listener", listener.name.as_str())],
            listener.bytes_received,
        )?;
        labels(
            &mut output,
            "oxiroute_listener_bytes_sent_total",
            &[("listener", listener.name.as_str())],
            listener.bytes_sent,
        )?;
        if let Some(http) = &listener.http_operations {
            for outcome in &http.outcomes {
                labels(
                    &mut output,
                    "oxiroute_http_requests_total",
                    &[
                        ("listener", listener.name.as_str()),
                        ("result", outcome.result.as_str()),
                    ],
                    outcome.count,
                )?;
            }
            render_latency_histogram(
                &mut output,
                "oxiroute_http_request_duration_milliseconds",
                listener.name.as_str(),
                &http.latency,
            )?;
        }
        if let Some(cache) = &listener.cache {
            labels(
                &mut output,
                "oxiroute_http_cache_hits_total",
                &[("listener", listener.name.as_str())],
                cache.hits,
            )?;
            labels(
                &mut output,
                "oxiroute_http_cache_misses_total",
                &[("listener", listener.name.as_str())],
                cache.misses,
            )?;
            labels(
                &mut output,
                "oxiroute_http_cache_admissions_total",
                &[("listener", listener.name.as_str())],
                cache.admissions,
            )?;
            labels(
                &mut output,
                "oxiroute_http_cache_evictions_total",
                &[("listener", listener.name.as_str())],
                cache.evictions,
            )?;
        }
        if let Some(tcp) = &listener.tcp_relays {
            for outcome in &tcp.outcomes {
                labels(
                    &mut output,
                    "oxiroute_tcp_relays_total",
                    &[
                        ("listener", listener.name.as_str()),
                        ("result", outcome.result.as_str()),
                    ],
                    outcome.count,
                )?;
            }
            render_latency_histogram(
                &mut output,
                "oxiroute_tcp_relay_duration_milliseconds",
                listener.name.as_str(),
                &tcp.latency,
            )?;
        }
        if let Some(proxy_protocol) = &listener.proxy_protocol {
            for outcome in &proxy_protocol.outcomes {
                labels(
                    &mut output,
                    "oxiroute_proxy_protocol_total",
                    &[
                        ("listener", listener.name.as_str()),
                        ("result", outcome.result.as_str()),
                    ],
                    outcome.count,
                )?;
            }
        }
    }

    let server_histograms = runtime
        .upstream_pools
        .iter()
        .map(|pool| pool.total_endpoints)
        .sum::<usize>()
        <= SERVER_HISTOGRAM_LIMIT;
    for pool in &runtime.upstream_pools {
        labels(
            &mut output,
            "oxiroute_pool_available_servers",
            &[("pool", pool.name.as_str())],
            pool.available_endpoints,
        )?;
        labels(
            &mut output,
            "oxiroute_pool_servers",
            &[("pool", pool.name.as_str())],
            pool.total_endpoints,
        )?;
        labels(
            &mut output,
            "oxiroute_pool_fail_open_selections_total",
            &[("pool", pool.name.as_str())],
            pool.fail_open_selections,
        )?;
        if let Some(canary) = &pool.canary {
            render_canary(&mut output, &pool.name, canary)?;
        }
        labels(
            &mut output,
            "oxiroute_pool_queue_depth",
            &[("pool", pool.name.as_str())],
            pool.queued,
        )?;
        for (class, depth) in &pool.queued_by_class {
            labels(
                &mut output,
                "oxiroute_pool_queue_class_depth",
                &[("pool", pool.name.as_str()), ("class", class.as_str())],
                *depth,
            )?;
        }
        labels(
            &mut output,
            "oxiroute_pool_queue_oldest_wait_seconds",
            &[("pool", pool.name.as_str())],
            pool.oldest_queue_wait_ms as f64 / 1_000.0,
        )?;
        render_histogram(
            &mut output,
            "oxiroute_pool_queue_wait_milliseconds",
            &[("pool", pool.name.as_str())],
            &crate::observability::upstream_stats::upstream_stats()
                .pool(&pool.name)
                .queue_wait(),
        )?;
        labels(
            &mut output,
            "oxiroute_pool_queue_timeouts_total",
            &[("pool", pool.name.as_str())],
            pool.queue_timeouts,
        )?;
        labels(
            &mut output,
            "oxiroute_pool_queue_admissions_total",
            &[("pool", pool.name.as_str())],
            pool.queued_total,
        )?;
        labels(
            &mut output,
            "oxiroute_pool_queue_cancellations_total",
            &[("pool", pool.name.as_str())],
            pool.queue_cancellations,
        )?;
        for server in &pool.endpoints {
            labels(
                &mut output,
                "oxiroute_server_health",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                    ("state", endpoint_state(server.state)),
                ],
                1,
            )?;
            labels(
                &mut output,
                "oxiroute_server_active_connections",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                ],
                server.active_connections,
            )?;
            labels(
                &mut output,
                "oxiroute_health_checks_total",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                    ("result", "success"),
                ],
                server.successful_checks,
            )?;
            labels(
                &mut output,
                "oxiroute_health_checks_total",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                    ("result", "failure"),
                ],
                server.failed_checks,
            )?;
            render_server_traffic(&mut output, pool.name.as_str(), server, server_histograms)?;
            let passive_reason = server
                .passive_ejection_reason
                .map_or("none", crate::HealthFailure::as_str);
            labels(
                &mut output,
                "oxiroute_server_passive_ejected",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                    ("reason", passive_reason),
                ],
                u8::from(server.passive_ejected),
            )?;
            labels(
                &mut output,
                "oxiroute_server_passive_failures_total",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                    ("reason", passive_reason),
                ],
                server.passive_failure_count,
            )?;
            labels(
                &mut output,
                "oxiroute_server_passive_ejections_total",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                ],
                server.passive_ejection_count,
            )?;
            labels(
                &mut output,
                "oxiroute_server_passive_recoveries_total",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                ],
                server.passive_recovery_count,
            )?;
            labels(
                &mut output,
                "oxiroute_server_passive_ejection_started_unix_seconds",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                ],
                server
                    .passive_ejected_at_unix_ms
                    .map_or(0, |timestamp| timestamp / 1_000),
            )?;
            labels(
                &mut output,
                "oxiroute_server_passive_ejection_until_unix_seconds",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                ],
                server
                    .passive_ejection_until_unix_ms
                    .map_or(0, |timestamp| timestamp / 1_000),
            )?;
            labels(
                &mut output,
                "oxiroute_server_passive_last_recovery_unix_seconds",
                &[
                    ("pool", pool.name.as_str()),
                    ("server", server.name.as_str()),
                ],
                server
                    .passive_last_recovery_at_unix_ms
                    .map_or(0, |timestamp| timestamp / 1_000),
            )?;
        }
    }

    render_inflight(&mut output)?;

    for certificate in &runtime.certbot_certificates {
        labels(
            &mut output,
            "oxiroute_certificate_info",
            &[("certificate", certificate.name.as_str())],
            1,
        )?;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(0, |duration| duration.as_secs());
    for certificate in &runtime.acme_managed_certificates {
        labels(
            &mut output,
            "oxiroute_acme_certificate_info",
            &[("certificate", certificate.name.as_str())],
            1,
        )?;
        if let Some(not_after) = certificate.not_after_unix_seconds {
            labels(
                &mut output,
                "oxiroute_acme_certificate_seconds_until_expiry",
                &[("certificate", certificate.name.as_str())],
                not_after.saturating_sub(now),
            )?;
        }
        labels(
            &mut output,
            "oxiroute_acme_renewal_due",
            &[("certificate", certificate.name.as_str())],
            u8::from(
                certificate
                    .next_action_unix_seconds
                    .is_some_and(|next| now >= next),
            ),
        )?;
        if let Some(outcome) = certificate.last_outcome.as_deref() {
            labels(
                &mut output,
                "oxiroute_acme_job_last_result",
                &[
                    ("certificate", certificate.name.as_str()),
                    ("result", outcome),
                ],
                1,
            )?;
        }
        if let Some(error) = certificate.last_error_code.as_deref() {
            labels(
                &mut output,
                "oxiroute_acme_job_last_error",
                &[("certificate", certificate.name.as_str()), ("code", error)],
                1,
            )?;
        }
        labels(
            &mut output,
            "oxiroute_acme_renewal_information_status",
            &[
                ("certificate", certificate.name.as_str()),
                ("status", certificate.renewal_information_status.as_str()),
            ],
            1,
        )?;
        labels(
            &mut output,
            "oxiroute_acme_dns_cleanup_status",
            &[
                ("certificate", certificate.name.as_str()),
                ("status", certificate.dns_cleanup_status.as_str()),
            ],
            1,
        )?;
        if let Some(provider) = certificate.dns_provider.as_deref() {
            if let Some(deployment) = certificate.dns_provider_deployment.as_deref() {
                labels(
                    &mut output,
                    "oxiroute_acme_dns_provider_deployment",
                    &[
                        ("certificate", certificate.name.as_str()),
                        ("provider", provider),
                        ("status", deployment),
                    ],
                    1,
                )?;
            }
            if let Some(health) = certificate.dns_provider_health.as_deref() {
                labels(
                    &mut output,
                    "oxiroute_acme_dns_provider_health",
                    &[
                        ("certificate", certificate.name.as_str()),
                        ("provider", provider),
                        ("status", health),
                    ],
                    1,
                )?;
            }
        }
    }

    let mut relay_attempts = 0_u64;
    let mut relay_reconnects = 0_u64;
    let mut relay_dns_refresh_attempts = 0_u64;
    let mut relay_dns_refresh_successes = 0_u64;
    let mut relay_dns_refresh_failures = 0_u64;
    let mut relay_drops = 0_u64;
    let mut recording_bytes = 0_u64;
    let mut recording_drops = 0_u64;
    let mut recording_queue = 0_usize;
    for stream in &rtmp.streams {
        for relay in &stream.relays {
            relay_attempts = relay_attempts.saturating_add(relay.status.connection_attempts);
            relay_reconnects = relay_reconnects.saturating_add(relay.status.reconnects);
            relay_dns_refresh_attempts =
                relay_dns_refresh_attempts.saturating_add(relay.status.dns_refresh_attempts);
            relay_dns_refresh_successes =
                relay_dns_refresh_successes.saturating_add(relay.status.dns_refresh_successes);
            relay_dns_refresh_failures =
                relay_dns_refresh_failures.saturating_add(relay.status.dns_refresh_failures);
            relay_drops = relay_drops.saturating_add(relay.status.events_dropped);
        }
        for recorder in &stream.recorders {
            recording_bytes = recording_bytes.saturating_add(recorder.bytes_written);
            recording_drops = recording_drops.saturating_add(recorder.events_dropped);
            recording_queue = recording_queue.saturating_add(recorder.queue_messages);
        }
    }
    sample(&mut output, "oxiroute_rtmp_streams", rtmp.streams.len())?;
    sample(&mut output, "oxiroute_rtmp_clients", rtmp_clients)?;
    let access_log = crate::observability::logging::rtmp_access_log_snapshot();
    sample(
        &mut output,
        "oxiroute_rtmp_access_log_queue_capacity",
        crate::observability::logging::RTMP_ACCESS_LOG_QUEUE_CAPACITY,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_access_log_queue_depth",
        access_log.queue_depth,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_access_log_events_enqueued_total",
        access_log.enqueued,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_access_log_events_written_total",
        access_log.written,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_access_log_events_dropped_total",
        access_log.dropped,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_access_log_queue_saturation_total",
        access_log.queue_saturated,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_access_log_write_failures_total",
        access_log.write_failures,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_relay_attempts_total",
        relay_attempts,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_relay_reconnects_total",
        relay_reconnects,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_relay_dns_refresh_attempts_total",
        relay_dns_refresh_attempts,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_relay_dns_refresh_successes_total",
        relay_dns_refresh_successes,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_relay_dns_refresh_failures_total",
        relay_dns_refresh_failures,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_relay_events_dropped_total",
        relay_drops,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_recording_bytes_total",
        recording_bytes,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_recording_events_dropped_total",
        recording_drops,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_recording_queue_depth",
        recording_queue,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_enabled",
        u8::from(auto_push.enabled),
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_started",
        u8::from(auto_push.started),
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_peers",
        auto_push.peers,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_source_streams",
        auto_push.source_streams,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_remote_streams",
        auto_push.remote_streams,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_frames_sent_total",
        auto_push.frames_sent,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_frames_received_total",
        auto_push.frames_received,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_frames_dropped_total",
        auto_push.frames_dropped,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_authentication_failures_total",
        auto_push.authentication_failures,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_reconnects_total",
        auto_push.reconnects,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_queue_messages",
        auto_push.queue_messages,
    )?;
    sample(
        &mut output,
        "oxiroute_rtmp_auto_push_queue_bytes",
        auto_push.queue_bytes,
    )?;

    sample(
        &mut output,
        "oxiroute_generation_prepares_total",
        generation.prepares,
    )?;
    sample(
        &mut output,
        "oxiroute_generation_activations_total",
        generation.activations,
    )?;
    sample(
        &mut output,
        "oxiroute_config_rejections_total",
        generation.config_rejections,
    )?;
    sample(
        &mut output,
        "oxiroute_generation_failures_total",
        generation.failures,
    )?;
    sample(
        &mut output,
        "oxiroute_generation_rollbacks_total",
        generation.rollbacks,
    )?;
    sample(
        &mut output,
        "oxiroute_generation_degraded",
        u8::from(generation.degraded),
    )?;
    if let Some(revision) = &generation.active_revision {
        labels(
            &mut output,
            "oxiroute_generation_info",
            &[
                ("generation", &generation.generation.to_string()),
                ("revision", revision.as_str()),
            ],
            1,
        )?;
    }
    render_retirement(&mut output, generations, &generation)?;
    labels(
        &mut output,
        "oxiroute_audit_component_state",
        &[("state", audit.status.state.as_str())],
        1,
    )?;
    sample(
        &mut output,
        "oxiroute_audit_persistent",
        u8::from(audit.status.persistent),
    )?;
    sample(
        &mut output,
        "oxiroute_audit_degraded",
        u8::from(audit.status.degraded),
    )?;
    sample(
        &mut output,
        "oxiroute_audit_records",
        audit.status.record_count,
    )?;
    sample(&mut output, "oxiroute_audit_bytes", audit.status.bytes)?;
    sample(
        &mut output,
        "oxiroute_audit_write_failures_total",
        audit.status.write_failures,
    )?;
    sample(
        &mut output,
        "oxiroute_audit_corrupt_records_total",
        audit.status.corrupt_records,
    )?;
    for category in crate::observability::operational_event::AuditCategory::ALL {
        for result in crate::observability::operational_event::AuditResult::ALL {
            labels(
                &mut output,
                "oxiroute_audit_operations_total",
                &[("category", category.as_str()), ("result", result.as_str())],
                audit.operation_counts[category.index()][result.index()],
            )?;
        }
    }
    if let Some(registry) = generations.registry() {
        registry::render_registry(&mut output, &registry.metrics())?;
    }
    Ok(output)
}

#[derive(Debug, thiserror::Error)]
pub enum PrometheusError {
    #[error("runtime metrics could not be sampled")]
    Metrics(#[from] crate::MetricsError),
    #[error("Prometheus exposition could not be formatted")]
    Format(#[from] fmt::Error),
}

#[cfg(test)]
mod tests;

fn render_retirement(
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
fn revision_label(revision: &str) -> &str {
    &revision[..revision.len().min(12)]
}

fn render_canary(
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
