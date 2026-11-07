use std::fmt::{self, Write as _};

pub(super) fn sample(output: &mut String, name: &str, value: impl fmt::Display) -> fmt::Result {
    writeln!(output, "{name} {value}")
}

pub(super) fn metric(
    output: &mut String,
    name: &str,
    label: &str,
    label_value: &str,
    value: impl fmt::Display,
) -> fmt::Result {
    labels(output, name, &[(label, label_value)], value)
}

pub(super) fn labels(
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

pub(super) fn render_histogram(
    output: &mut String,
    name: &str,
    base_labels: &[(&str, &str)],
    latency: &crate::LatencySnapshot,
) -> fmt::Result {
    let bucket_name = format!("{name}_bucket");
    for bucket in &latency.buckets {
        let upper_bound = bucket
            .upper_bound_ms
            .map_or_else(|| "+Inf".to_owned(), |value| value.to_string());
        let mut bucket_labels = base_labels.to_vec();
        bucket_labels.push(("le", upper_bound.as_str()));
        labels(output, &bucket_name, &bucket_labels, bucket.count)?;
    }
    labels(output, &format!("{name}_count"), base_labels, latency.count)?;
    labels(output, &format!("{name}_sum"), base_labels, latency.sum_ms)
}

pub(super) fn render_latency_histogram(
    output: &mut String,
    name: &str,
    listener: &str,
    latency: &crate::LatencySnapshot,
) -> fmt::Result {
    render_histogram(output, name, &[("listener", listener)], latency)
}

pub(super) fn render_transport_latency_histogram(
    output: &mut String,
    name: &str,
    transport: &str,
    latency: &crate::LatencySnapshot,
) -> fmt::Result {
    render_histogram(output, name, &[("transport", transport)], latency)
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

pub(super) const fn listener_state(state: crate::ListenerRuntimeState) -> &'static str {
    match state {
        crate::ListenerRuntimeState::Configured => "configured",
        crate::ListenerRuntimeState::Listening => "listening",
        crate::ListenerRuntimeState::Stopped => "stopped",
        crate::ListenerRuntimeState::Failed => "failed",
    }
}

pub(super) const fn endpoint_state(state: crate::EndpointHealthState) -> &'static str {
    match state {
        crate::EndpointHealthState::Unchecked => "unchecked",
        crate::EndpointHealthState::Unknown => "unknown",
        crate::EndpointHealthState::Healthy => "healthy",
        crate::EndpointHealthState::Unhealthy => "unhealthy",
    }
}

pub(super) const fn component_state(state: crate::monitoring::ComponentState) -> &'static str {
    match state {
        crate::monitoring::ComponentState::Healthy => "healthy",
        crate::monitoring::ComponentState::Degraded => "degraded",
        crate::monitoring::ComponentState::Unsupported => "unsupported",
    }
}
