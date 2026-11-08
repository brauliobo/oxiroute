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
    series(output, name, "", labels, None, value)
}

/// The escaped `key="value"` pairs that identify one object, written once for all of its series.
pub(super) fn label_prefix(labels: &[(&str, &str)]) -> String {
    let mut prefix = String::new();
    for (index, (key, label)) in labels.iter().enumerate() {
        if index > 0 {
            prefix.push(',');
        }
        prefix.push_str(key);
        prefix.push_str("=\"");
        escape_label(&mut prefix, label);
        prefix.push('"');
    }
    prefix
}

/// One sample line of an object whose labels are already in `prefix`, plus an optional `extra` label.
pub(super) fn with_prefix(
    output: &mut String,
    name: &str,
    prefix: &str,
    extra: Option<(&str, &str)>,
    value: u64,
) {
    output.push_str(name);
    output.push('{');
    output.push_str(prefix);
    if let Some((key, label)) = extra {
        output.push(',');
        output.push_str(key);
        output.push_str("=\"");
        escape_label(output, label);
        output.push('"');
    }
    output.push_str("} ");
    push_integer(output, value);
    output.push('\n');
}

/// Writes `value` in decimal without going through the formatting machinery.
fn push_integer(output: &mut String, mut value: u64) {
    let mut digits = [b'0'; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] += u8::try_from(value % 10).expect("a digit");
        value /= 10;
        if value == 0 {
            break;
        }
    }
    output.push_str(std::str::from_utf8(&digits[start..]).expect("ascii digits"));
}

/// One sample line: `name` with `suffix`, the `base` labels, then `extra` if present.
fn series(
    output: &mut String,
    name: &str,
    suffix: &str,
    base: &[(&str, &str)],
    extra: Option<(&str, &str)>,
    value: impl fmt::Display,
) -> fmt::Result {
    output.push_str(name);
    output.push_str(suffix);
    output.push('{');
    for (index, (key, label)) in base.iter().chain(&extra).enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(key);
        output.push_str("=\"");
        escape_label(output, label);
        output.push('"');
    }
    output.push_str("} ");
    writeln!(output, "{value}")
}

pub(super) fn render_histogram(
    output: &mut String,
    name: &str,
    base_labels: &[(&str, &str)],
    latency: &crate::LatencySnapshot,
) -> fmt::Result {
    let mut upper_bound = String::new();
    for bucket in &latency.buckets {
        upper_bound.clear();
        match bucket.upper_bound_ms {
            Some(value) => write!(upper_bound, "{value}")?,
            None => upper_bound.push_str("+Inf"),
        }
        let extra = Some(("le", upper_bound.as_str()));
        series(output, name, "_bucket", base_labels, extra, bucket.count)?;
    }
    series(output, name, "_count", base_labels, None, latency.count)?;
    series(output, name, "_sum", base_labels, None, latency.sum_ms)
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
    if !value
        .bytes()
        .any(|byte| matches!(byte, b'\\' | b'"' | b'\n'))
    {
        output.push_str(value);
        return;
    }
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

pub(super) const fn component_state(
    state: crate::observability::monitoring::ComponentState,
) -> &'static str {
    match state {
        crate::observability::monitoring::ComponentState::Healthy => "healthy",
        crate::observability::monitoring::ComponentState::Degraded => "degraded",
        crate::observability::monitoring::ComponentState::Unsupported => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_series_match_the_general_writer() {
        let (mut general, mut prefixed) = (String::new(), String::new());
        let pool = "we\"ird\\pool\n";
        let prefix = label_prefix(&[("pool", pool), ("server", "a")]);
        for value in [0, 7, 10, 1_234_567_890_123, u64::MAX] {
            labels(
                &mut general,
                "m",
                &[("pool", pool), ("server", "a"), ("state", "up")],
                value,
            )
            .expect("write");
            with_prefix(&mut prefixed, "m", &prefix, Some(("state", "up")), value);
        }
        assert_eq!(prefixed, general);
        assert!(general.contains(
            "m{pool=\"we\\\"ird\\\\pool\\n\",server=\"a\",state=\"up\"} 18446744073709551615\n"
        ));
    }
}
