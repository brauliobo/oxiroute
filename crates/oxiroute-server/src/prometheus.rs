use std::fmt::{self, Write as _};

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
    let generation = generations.status();
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
    use oxiroute_rtmp::RtmpCapabilities;

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
        assert!(!output.contains("192.0.2.10"));
        assert!(!output.contains("private-port"));
    }
}
