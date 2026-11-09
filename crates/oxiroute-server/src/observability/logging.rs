use std::{
    borrow::Cow,
    sync::{
        Arc, LazyLock, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use aho_corasick::AhoCorasick;
use serde_json::{Map, Value};

pub(crate) const REDACTED: &str = "<redacted>";
pub(crate) const RTMP_ACCESS_LOG_QUEUE_CAPACITY: u64 = 1_024;

const MAX_SAFE_TEXT_BYTES: usize = 128;
/// No marker is shorter, so shorter text (methods, protocols, empty reasons) needs no search.
const MIN_MARKER_BYTES: usize = 5;
const SENSITIVE_MARKERS: &[&str] = &[
    "authorization",
    "bearer ",
    "cookie",
    "credential",
    "challenge",
    "password",
    "private-key",
    "private_key",
    "privatekey",
    "request-body",
    "request_body",
    "requestbody",
    "secret",
    "token",
];

/// Control characters removed and the length bounded; borrowed when the text is already clean.
fn bounded_text(value: &str) -> Cow<'_, str> {
    if value.len() <= MAX_SAFE_TEXT_BYTES && !value.chars().any(char::is_control) {
        return Cow::Borrowed(value);
    }
    Cow::Owned(
        value
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_SAFE_TEXT_BYTES)
            .collect(),
    )
}

fn is_sensitive(text: &str) -> bool {
    static MARKERS: LazyLock<AhoCorasick> = LazyLock::new(|| {
        AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(SENSITIVE_MARKERS)
            .expect("sensitive markers compile")
    });
    text.len() >= MIN_MARKER_BYTES && MARKERS.is_match(text)
}

/// Allocation-free [`redact_text`] for the per-request path.
pub(crate) fn redact_text_cow(value: &str) -> Cow<'_, str> {
    let bounded = bounded_text(value);
    if is_sensitive(&bounded) {
        Cow::Borrowed(REDACTED)
    } else {
        bounded
    }
}

pub(crate) fn redact_text(value: &str) -> String {
    redact_text_cow(value).into_owned()
}

pub(crate) fn redact_identifier(value: &str) -> String {
    let value = redact_text(value);
    if value.is_empty() {
        "unknown".to_owned()
    } else {
        value
    }
}

/// A process-unique correlation id that renders without allocating.
pub(crate) struct CorrelationId(u64);

impl CorrelationId {
    pub(crate) fn next() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for CorrelationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "op-{:08x}-{:016x}", std::process::id(), self.0)
    }
}

pub(crate) fn next_correlation_id() -> String {
    CorrelationId::next().to_string()
}

pub(crate) fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[derive(Debug, Default)]
pub(crate) struct RtmpAccessLogMetrics {
    queue_depth: AtomicU64,
    enqueued: AtomicU64,
    written: AtomicU64,
    dropped: AtomicU64,
    queue_saturated: AtomicU64,
    write_failures: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RtmpAccessLogMetricsSnapshot {
    pub(crate) queue_depth: u64,
    pub(crate) enqueued: u64,
    pub(crate) written: u64,
    pub(crate) dropped: u64,
    pub(crate) queue_saturated: u64,
    pub(crate) write_failures: u64,
}

impl RtmpAccessLogMetrics {
    pub(crate) fn queue_event(&self) {
        self.queue_depth.fetch_add(1, Ordering::Relaxed);
        self.enqueued.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn queue_event_rejected(&self, saturated: bool) {
        self.decrement_queue_depth(1);
        self.enqueued.fetch_sub(1, Ordering::Relaxed);
        self.dropped.fetch_add(1, Ordering::Relaxed);
        if saturated {
            self.queue_saturated.fetch_add(1, Ordering::Relaxed);
        } else {
            self.write_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn worker_received(&self, lines: u64) {
        self.decrement_queue_depth(lines);
    }

    fn decrement_queue_depth(&self, lines: u64) {
        let _ = self
            .queue_depth
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |depth| {
                Some(depth.saturating_sub(lines))
            });
    }

    pub(crate) fn worker_written(&self, lines: u64) {
        self.written.fetch_add(lines, Ordering::Relaxed);
    }

    pub(crate) fn worker_failed(&self) {
        self.write_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> RtmpAccessLogMetricsSnapshot {
        RtmpAccessLogMetricsSnapshot {
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            enqueued: self.enqueued.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            queue_saturated: self.queue_saturated.load(Ordering::Relaxed),
            write_failures: self.write_failures.load(Ordering::Relaxed),
        }
    }
}

pub(crate) fn rtmp_access_log_metrics() -> Arc<RtmpAccessLogMetrics> {
    static METRICS: OnceLock<Arc<RtmpAccessLogMetrics>> = OnceLock::new();
    Arc::clone(METRICS.get_or_init(|| Arc::new(RtmpAccessLogMetrics::default())))
}

pub(crate) fn rtmp_access_log_snapshot() -> RtmpAccessLogMetricsSnapshot {
    rtmp_access_log_metrics().snapshot()
}

/// Keeps access records to a fixed metadata allowlist before they reach a configured sink.
pub(crate) fn redact_access_record(value: &Value) -> Value {
    const SAFE_FIELDS: &[&str] = &[
        "timestampUnixMs",
        "event",
        "outcome",
        "service",
        "route",
        "host",
        "protocol",
        "method",
        "status",
        "bytesReceived",
        "bytesSent",
        "durationMs",
        "result",
        "reason",
        "authenticated",
        "requestId",
        "idemKey",
        "pool",
        "server",
        "servers",
        "attempts",
        "evacuations",
        "queueWaitMs",
        "connectMs",
        "headerMs",
        "rescued",
        "wasStuck",
        "phase",
        "ageMs",
    ];

    let mut record = Map::new();
    if let Value::Object(fields) = value {
        for (name, value) in fields {
            if SAFE_FIELDS.contains(&name.as_str()) {
                record.insert(name.clone(), redact_access_value(name, value));
            }
        }
    }
    let correlation_id = value
        .get("correlationId")
        .and_then(Value::as_str)
        .filter(|value| valid_correlation_id(value))
        .map_or_else(next_correlation_id, ToOwned::to_owned);
    record.insert("correlationId".into(), Value::String(correlation_id));
    Value::Object(record)
}

fn redact_access_value(name: &str, value: &Value) -> Value {
    match value {
        Value::String(value) => {
            if matches!(name, "event" | "outcome" | "protocol" | "result") {
                Value::String(redact_identifier(value))
            } else {
                Value::String(redact_text(value))
            }
        }
        Value::Array(_) | Value::Object(_) => Value::String(REDACTED.to_owned()),
        value => value.clone(),
    }
}

/// Keeps only the fixed RTMP access-event contract. Stream queries, payloads, addresses, and
/// arbitrary fields never cross this boundary.
pub(crate) fn redact_rtmp_access_record(value: &Value) -> Value {
    const SAFE_FIELDS: &[&str] = &[
        "timestampUnixMs",
        "event",
        "result",
        "listener",
        "service",
        "application",
        "stream",
        "sessionId",
        "role",
        "bytesReceived",
        "bytesSent",
        "messagesReceived",
        "messagesSent",
        "durationMs",
        "failureCode",
    ];

    let mut record = Map::new();
    if let Value::Object(fields) = value {
        for name in SAFE_FIELDS {
            if let Some(value) = fields.get(*name) {
                record.insert((*name).to_owned(), redact_rtmp_access_value(name, value));
            }
        }
    }
    let correlation_id = value
        .get("correlationId")
        .and_then(Value::as_str)
        .filter(|value| valid_correlation_id(value))
        .map_or_else(next_correlation_id, ToOwned::to_owned);
    record.insert("correlationId".into(), Value::String(correlation_id));
    Value::Object(record)
}

fn redact_rtmp_access_value(name: &str, value: &Value) -> Value {
    match value {
        Value::String(value) => Value::String(redact_identifier(value)),
        Value::Number(_)
            if matches!(
                name,
                "timestampUnixMs"
                    | "bytesReceived"
                    | "bytesSent"
                    | "messagesReceived"
                    | "messagesSent"
                    | "durationMs"
            ) =>
        {
            value.clone()
        }
        Value::Array(_) | Value::Object(_) => Value::String(REDACTED.to_owned()),
        _ => Value::Null,
    }
}

pub(crate) fn log_json(target: &'static str, value: &Value) {
    if let Ok(line) = serde_json::to_string(value) {
        log::info!(target: target, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        MIN_MARKER_BYTES, REDACTED, RtmpAccessLogMetrics, SENSITIVE_MARKERS, redact_access_record,
        redact_identifier, redact_rtmp_access_record, redact_text, valid_correlation_id,
    };

    #[test]
    fn the_shortest_marker_sets_the_search_threshold() {
        let shortest = SENSITIVE_MARKERS.iter().map(|marker| marker.len()).min();
        assert_eq!(shortest, Some(MIN_MARKER_BYTES));
        assert_eq!(redact_text("token"), REDACTED);
        assert_eq!(redact_text("toke"), "toke");
    }

    #[test]
    fn redacts_secret_bearing_log_values() {
        for value in [
            "Authorization: Bearer access-token",
            "Cookie=session-cookie",
            "private-key-secret",
            "dns challenge value",
            "request_body={\"password\":\"value\"}",
        ] {
            assert_eq!(redact_text(value), REDACTED);
            assert!(!redact_text(value).contains("token"));
        }
    }

    #[test]
    fn bounds_non_secret_identifiers_and_removes_controls() {
        let value = format!("pool\n{}", "x".repeat(256));
        let value = redact_identifier(&value);
        assert!(!value.contains('\n'));
        assert!(value.len() <= 128);
    }

    #[test]
    fn access_records_drop_uri_body_and_credentials_and_add_safe_correlation() {
        let record = redact_access_record(&json!({
            "event": "http_access",
            "service": "edge",
            "protocol": "h3",
            "method": "GET",
            "uri": "/private?token=secret",
            "query": "token=secret",
            "requestBody": "password=secret",
            "authorization": "Bearer secret",
            "status": 200,
        }));

        assert_eq!(record["event"], "http_access");
        assert_eq!(record["status"], 200);
        assert!(record.get("uri").is_none());
        assert!(record.get("query").is_none());
        assert!(record.get("requestBody").is_none());
        assert!(record.get("authorization").is_none());
        assert!(valid_correlation_id(
            record["correlationId"].as_str().expect("correlation ID")
        ));
        assert!(!record.to_string().contains("secret"));
    }

    #[test]
    fn rtmp_records_keep_only_the_fixed_redacted_schema() {
        let record = redact_rtmp_access_record(&json!({
            "timestampUnixMs": 42,
            "event": "publish",
            "result": "accepted",
            "listener": "live-listener",
            "service": "live",
            "application": "camera",
            "stream": "feed",
            "sessionId": "session-1",
            "role": "publisher",
            "bytesReceived": 123,
            "bytesSent": 456,
            "messagesReceived": 7,
            "messagesSent": 8,
            "durationMs": 9,
            "failureCode": null,
            "clientIp": "192.0.2.1",
            "query": "token=secret",
            "token": "secret",
            "payload": "raw-secret-payload",
            "arbitrary": "not allowed",
        }));

        assert_eq!(record["stream"], "feed");
        assert_eq!(record["bytesReceived"], 123);
        assert_eq!(record["failureCode"], Value::Null);
        assert!(record.get("clientIp").is_none());
        assert!(record.get("query").is_none());
        assert!(record.get("token").is_none());
        assert!(record.get("payload").is_none());
        assert!(record.get("arbitrary").is_none());
        assert!(valid_correlation_id(
            record["correlationId"].as_str().unwrap()
        ));
        assert!(!record.to_string().contains("secret"));
    }

    #[test]
    fn rtmp_queue_depth_does_not_underflow_when_rejection_races_delivery() {
        let metrics = RtmpAccessLogMetrics::default();
        metrics.queue_event();
        metrics.worker_received(1);
        metrics.queue_event_rejected(true);

        assert_eq!(metrics.snapshot().queue_depth, 0);
    }
}
