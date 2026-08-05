use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

pub(crate) const REDACTED: &str = "<redacted>";

const MAX_SAFE_TEXT_BYTES: usize = 128;
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

pub(crate) fn redact_text(value: &str) -> String {
    let bounded: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_SAFE_TEXT_BYTES)
        .collect();
    let normalized = bounded.to_ascii_lowercase();
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        REDACTED.to_owned()
    } else {
        bounded
    }
}

pub(crate) fn redact_identifier(value: &str) -> String {
    let value = redact_text(value);
    if value.is_empty() {
        "unknown".to_owned()
    } else {
        value
    }
}

pub(crate) fn next_correlation_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("op-{:08x}-{:016x}", std::process::id(), sequence)
}

pub(crate) fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
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

pub(crate) fn log_json(target: &'static str, value: &Value) {
    if let Ok(line) = serde_json::to_string(value) {
        log::info!(target: target, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{redact_access_record, redact_identifier, redact_text, valid_correlation_id, REDACTED};

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
        assert!(valid_correlation_id(record["correlationId"].as_str().expect("correlation ID")));
        assert!(!record.to_string().contains("secret"));
    }
}
