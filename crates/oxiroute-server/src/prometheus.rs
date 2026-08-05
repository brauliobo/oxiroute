use std::{
    fmt::{self, Write as _},
    time::{SystemTime, UNIX_EPOCH},
};

use oxiroute_rtmp::RtmpRegistry;

use crate::{GenerationManager, RuntimeMetrics};

/// Renders a Prometheus text exposition without endpoint addresses, paths, stream names, or secret
/// material in labels.
///
/// # Errors
///
/// Returns a metrics sampling or formatting error.
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
pub fn render_prometheus(
    metrics: &RuntimeMetrics,
    registry: &RtmpRegistry,
    generations: &GenerationManager,
) -> Result<String, PrometheusError> {
    let runtime = metrics.snapshot()?;
    let rtmp = registry.snapshot();
    let auto_push = generations
        .active()
        .map_or_else(oxiroute_rtmp::RtmpAutoPushStatus::default, |generation| {
            generation.rtmp_auto_push_status()
        });
    let rtmp_clients = registry
        .session_snapshots()
        .into_iter()
        .filter(|client| client.connected)
        .count();
    let generation = generations.status();
    let audit = crate::operational_event::audit_metrics();
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
    sample(
        &mut output,
        "oxiroute_process_resident_memory_bytes",
        runtime.process.resident_memory_bytes,
    )?;
    if let Some(cpu) = runtime.process.cpu_percent {
        sample(&mut output, "oxiroute_process_cpu_percent", cpu)?;
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
            "oxiroute_pool_queue_depth",
            &[("pool", pool.name.as_str())],
            pool.queued,
        )?;
        labels(
            &mut output,
            "oxiroute_pool_queue_timeouts_total",
            &[("pool", pool.name.as_str())],
            pool.queue_timeouts,
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
    }

    let mut relay_attempts = 0_u64;
    let mut relay_reconnects = 0_u64;
    let mut relay_drops = 0_u64;
    let mut recording_bytes = 0_u64;
    let mut recording_drops = 0_u64;
    let mut recording_queue = 0_usize;
    for stream in &rtmp.streams {
        for relay in &stream.relays {
            relay_attempts = relay_attempts.saturating_add(relay.status.connection_attempts);
            relay_reconnects = relay_reconnects.saturating_add(relay.status.reconnects);
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
    for category in crate::operational_event::AuditCategory::ALL {
        for result in crate::operational_event::AuditResult::ALL {
            labels(
                &mut output,
                "oxiroute_audit_operations_total",
                &[("category", category.as_str()), ("result", result.as_str())],
                audit.operation_counts[category.index()][result.index()],
            )?;
        }
    }
    Ok(output)
}

fn sample(output: &mut String, name: &str, value: impl fmt::Display) -> fmt::Result {
    writeln!(output, "{name} {value}")
}

fn metric(
    output: &mut String,
    name: &str,
    label: &str,
    label_value: &str,
    value: impl fmt::Display,
) -> fmt::Result {
    labels(output, name, &[(label, label_value)], value)
}

fn labels(
    output: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    value: impl fmt::Display,
) -> fmt::Result {
    write!(output, "{name}{{")?;
    for (index, (key, value)) in labels.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(output, "{key}=\"")?;
        escape_label(output, value);
        output.push('"');
    }
    writeln!(output, "}} {value}")
}

fn render_latency_histogram(
    output: &mut String,
    name: &str,
    listener: &str,
    latency: &crate::LatencySnapshot,
) -> fmt::Result {
    let bucket_name = format!("{name}_bucket");
    let count_name = format!("{name}_count");
    let sum_name = format!("{name}_sum");
    for bucket in &latency.buckets {
        let upper_bound = bucket
            .upper_bound_ms
            .map_or_else(|| "+Inf".to_owned(), |value| value.to_string());
        labels(
            output,
            &bucket_name,
            &[("listener", listener), ("le", upper_bound.as_str())],
            bucket.count,
        )?;
    }
    labels(
        output,
        &count_name,
        &[("listener", listener)],
        latency.count,
    )?;
    labels(output, &sum_name, &[("listener", listener)], latency.sum_ms)
}

fn escape_label(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            character => output.push(character),
        }
    }
}

const fn listener_state(state: crate::ListenerRuntimeState) -> &'static str {
    match state {
        crate::ListenerRuntimeState::Configured => "configured",
        crate::ListenerRuntimeState::Listening => "listening",
        crate::ListenerRuntimeState::Stopped => "stopped",
        crate::ListenerRuntimeState::Failed => "failed",
    }
}

const fn endpoint_state(state: crate::EndpointHealthState) -> &'static str {
    match state {
        crate::EndpointHealthState::Unchecked => "unchecked",
        crate::EndpointHealthState::Unknown => "unknown",
        crate::EndpointHealthState::Healthy => "healthy",
        crate::EndpointHealthState::Unhealthy => "unhealthy",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrometheusError {
    #[error("runtime metrics could not be sampled")]
    Metrics(#[from] crate::MetricsError),
    #[error("Prometheus exposition could not be formatted")]
    Format(#[from] fmt::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oxiroute_config::UpstreamAlgorithm;
    use oxiroute_rtmp::RtmpCapabilities;

    use crate::{HealthFailure, RoundRobinPool, RuntimeEndpoint};

    use super::*;

    #[test]
    fn exposition_contains_operational_families_without_listener_binds() {
        let metrics = RuntimeMetrics::new();
        metrics
            .register_listener(
                "public\"edge",
                "http",
                "socket:192.0.2.10:private-port",
                None,
            )
            .expect("listener");
        let registry = RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        });
        let output =
            render_prometheus(&metrics, &registry, &GenerationManager::new()).expect("exposition");

        assert!(output.contains("oxiroute_process_uptime_seconds"));
        assert!(output.contains("listener=\"public\\\"edge\""));
        assert!(output.contains("oxiroute_generation_activations_total"));
        assert!(output.contains("oxiroute_audit_component_state{state=\"memory\"} 1"));
        assert!(output.contains(
            "oxiroute_audit_operations_total{category=\"reload\",result=\"requested\"} 0"
        ));
        assert!(!output.contains("192.0.2.10"));
        assert!(!output.contains("private-port"));
    }

    #[test]
    fn exposition_contains_bounded_http_and_tcp_outcome_histograms() {
        let metrics = RuntimeMetrics::new();
        let listener = metrics
            .register_listener("edge", "http", "socket:192.0.2.10:private-port", None)
            .expect("listener");
        listener
            .record_http_operation(
                crate::HttpOperationResult::Success,
                std::time::Duration::from_millis(7),
            )
            .expect("HTTP result");
        listener
            .record_http_operation(
                crate::HttpOperationResult::UpstreamError,
                std::time::Duration::from_millis(65),
            )
            .expect("HTTP result");
        listener
            .record_tcp_relay(
                crate::TcpRelayResult::IdleTimeout,
                std::time::Duration::from_secs(2),
            )
            .expect("TCP result");
        let registry = RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        });

        let output =
            render_prometheus(&metrics, &registry, &GenerationManager::new()).expect("exposition");

        assert!(
            output.contains("oxiroute_http_requests_total{listener=\"edge\",result=\"success\"} 1")
        );
        assert!(output.contains(
            "oxiroute_http_requests_total{listener=\"edge\",result=\"upstream_error\"} 1"
        ));
        assert!(output.contains(
            "oxiroute_http_request_duration_milliseconds_bucket{listener=\"edge\",le=\"10\"} 1"
        ));
        assert!(output
            .contains("oxiroute_tcp_relays_total{listener=\"edge\",result=\"idle_timeout\"} 1"));
        assert!(output.contains(
            "oxiroute_tcp_relay_duration_milliseconds_bucket{listener=\"edge\",le=\"5000\"} 1"
        ));
        assert!(!output.contains("/sensitive?token="));
    }

    #[test]
    fn exposition_contains_bounded_passive_endpoint_observability() {
        let metrics = RuntimeMetrics::new();
        let pool = Arc::new(
            RoundRobinPool::new_named(
                "observability".into(),
                [RuntimeEndpoint::from(
                    "127.0.0.1:3000"
                        .parse::<std::net::SocketAddr>()
                        .expect("endpoint"),
                )],
                UpstreamAlgorithm::RoundRobin,
                false,
            )
            .expect("pool"),
        );
        for _ in 0..3 {
            pool.record_passive_failure(0, HealthFailure::ConnectFailed);
        }
        metrics
            .register_upstream_pools([Arc::clone(&pool)])
            .expect("upstream pools");
        let registry = RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        });

        let output =
            render_prometheus(&metrics, &registry, &GenerationManager::new()).expect("exposition");

        assert!(output.contains(
            "oxiroute_server_passive_ejected{pool=\"observability\",server=\"0\",reason=\"connect_failed\"} 1"
        ));
        assert!(output.contains(
            "oxiroute_server_passive_failures_total{pool=\"observability\",server=\"0\",reason=\"connect_failed\"} 3"
        ));
        assert!(output.contains(
            "oxiroute_server_passive_ejections_total{pool=\"observability\",server=\"0\"} 1"
        ));
        assert!(!output.contains("127.0.0.1:3000"));
    }
}
