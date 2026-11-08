//! Typed HTTP access-log records.
//!
//! Each struct is the allowlist of its record kind: a value can only reach the sink through one of its
//! fields, and every text field is redacted while it is written, so the hot path builds no intermediate
//! JSON tree. Fields are declared in key order, which keeps the lines identical to the sorted maps the
//! redaction pass used to produce. Large counters are strings, like the rest of the log.

use std::{borrow::Cow, fmt::Display};

use serde::{Serialize, Serializer};

use super::logging::{CorrelationId, redact_text_cow, valid_correlation_id};

/// Free text, bounded and replaced by a marker when it looks like a credential.
pub(crate) struct Safe<'a>(Cow<'a, str>);

impl<'a, T: Into<Cow<'a, str>>> From<T> for Safe<'a> {
    fn from(text: T) -> Self {
        Self(text.into())
    }
}

impl Serialize for Safe<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&redact_text_cow(&self.0))
    }
}

/// A short enumerated value; empty becomes `unknown` so the field is never blank.
pub(crate) struct SafeId<'a>(pub(crate) &'a str);

impl Serialize for SafeId<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match redact_text_cow(self.0) {
            text if text.is_empty() => serializer.serialize_str("unknown"),
            text => serializer.serialize_str(&text),
        }
    }
}

/// A number written as a decimal string.
pub(crate) struct Count<T>(pub(crate) T);

impl<T: Display> Serialize for Count<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

/// The caller's request id when it is a valid identifier, a fresh one otherwise.
pub(crate) struct Correlation<'a>(pub(crate) Option<&'a str>);

impl Serialize for Correlation<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.filter(|id| valid_correlation_id(id)) {
            Some(id) => serializer.serialize_str(id),
            None => serializer.collect_str(&CorrelationId::next()),
        }
    }
}

/// One line per completed request on a reverse HTTP/1.1 or HTTP/2 listener.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HttpAccess<'a> {
    pub(crate) attempts: usize,
    pub(crate) bytes_received: Count<u64>,
    pub(crate) bytes_sent: Count<u64>,
    pub(crate) connect_ms: Option<Count<u128>>,
    pub(crate) correlation_id: Correlation<'a>,
    pub(crate) duration_ms: Count<u128>,
    pub(crate) evacuations: usize,
    pub(crate) header_ms: Option<Count<u128>>,
    pub(crate) host: Option<Safe<'a>>,
    pub(crate) idem_key: Option<Safe<'a>>,
    pub(crate) method: Safe<'a>,
    pub(crate) pool: Option<Safe<'a>>,
    pub(crate) protocol: SafeId<'a>,
    pub(crate) queue_wait_ms: Count<u128>,
    pub(crate) reason: Safe<'a>,
    pub(crate) request_id: Option<Safe<'a>>,
    pub(crate) rescued: bool,
    pub(crate) route: Option<Safe<'a>>,
    pub(crate) server: Option<Safe<'a>>,
    pub(crate) servers: Option<Safe<'a>>,
    pub(crate) service: Safe<'a>,
    pub(crate) status: Option<u16>,
    pub(crate) timestamp_unix_ms: Option<u64>,
    pub(crate) was_stuck: bool,
}

/// One line per completed request on a reverse HTTP/3 listener.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Http3Access<'a> {
    pub(crate) correlation_id: Correlation<'a>,
    pub(crate) duration_ms: Count<u128>,
    pub(crate) host: Option<Safe<'a>>,
    pub(crate) method: Safe<'a>,
    pub(crate) protocol: SafeId<'a>,
    pub(crate) service: Safe<'a>,
    pub(crate) status: Option<u16>,
    pub(crate) timestamp_unix_ms: Option<u64>,
}

/// The arrival line of a request that went quiet, which a completion-only log cannot give.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StuckAccess<'a> {
    pub(crate) age_ms: Count<u64>,
    pub(crate) correlation_id: Correlation<'a>,
    pub(crate) event: SafeId<'a>,
    pub(crate) phase: Safe<'a>,
    pub(crate) pool: Option<Safe<'a>>,
    pub(crate) request_id: Option<Safe<'a>>,
    pub(crate) route: Option<Safe<'a>>,
    pub(crate) server: Option<Safe<'a>>,
    pub(crate) service: Safe<'a>,
    pub(crate) timestamp_unix_ms: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::observability::logging::{REDACTED, redact_access_record};

    fn http_access() -> HttpAccess<'static> {
        HttpAccess {
            attempts: 2,
            bytes_received: Count(10),
            bytes_sent: Count(2_000_000_000_000),
            connect_ms: Some(Count(3)),
            correlation_id: Correlation(Some("req-1")),
            duration_ms: Count(15),
            evacuations: 1,
            header_ms: None,
            host: Some(Safe::from("app.example.com")),
            idem_key: None,
            method: Safe::from("GET"),
            pool: Some(Safe::from("farm")),
            protocol: SafeId("http/1.1"),
            queue_wait_ms: Count(0),
            reason: Safe::from(""),
            request_id: Some(Safe::from("req-1")),
            rescued: false,
            route: Some(Safe::from("api")),
            server: Some(Safe::from("gpu-2")),
            servers: Some(Safe::from("gpu-1,gpu-2")),
            service: Safe::from("edge"),
            status: Some(200),
            timestamp_unix_ms: Some(1_700_000_000_000),
            was_stuck: false,
        }
    }

    #[test]
    fn http_lines_keep_the_documented_shape() {
        let line = serde_json::to_string(&http_access()).expect("serialize");
        assert_eq!(
            line,
            concat!(
                r#"{"attempts":2,"bytesReceived":"10","bytesSent":"2000000000000","connectMs":"3","#,
                r#""correlationId":"req-1","durationMs":"15","evacuations":1,"headerMs":null,"#,
                r#""host":"app.example.com","idemKey":null,"method":"GET","pool":"farm","#,
                r#""protocol":"http/1.1","queueWaitMs":"0","reason":"","requestId":"req-1","#,
                r#""rescued":false,"route":"api","server":"gpu-2","servers":"gpu-1,gpu-2","#,
                r#""service":"edge","status":200,"timestampUnixMs":1700000000000,"wasStuck":false}"#,
            )
        );
    }

    #[test]
    fn typed_lines_match_the_redacted_map_for_the_same_event() {
        let record = http_access();
        let event = json!({
            "timestampUnixMs": 1_700_000_000_000_u64, "correlationId": "req-1", "requestId": "req-1",
            "idemKey": null, "service": "edge", "route": "api", "host": "app.example.com", "method": "GET",
            "protocol": "http/1.1", "status": 200, "reason": "", "pool": "farm", "server": "gpu-2",
            "servers": "gpu-1,gpu-2", "attempts": 2, "evacuations": 1, "rescued": false,
            "wasStuck": false, "queueWaitMs": "0", "connectMs": "3", "headerMs": null,
            "bytesReceived": "10", "bytesSent": "2000000000000", "durationMs": "15",
            "clientIp": "192.0.2.7",
        });
        let typed = serde_json::to_string(&record).expect("serialize");
        assert_eq!(
            typed,
            serde_json::to_string(&redact_access_record(&event)).expect("serialize")
        );
    }

    #[test]
    fn text_is_redacted_bounded_and_stripped_of_controls() {
        let mut record = http_access();
        record.host = Some(Safe::from("Authorization: Bearer abc"));
        record.pool = Some(Safe::from("farm\nbreak"));
        record.server = Some(Safe::from("é"));
        record.servers = Some(Safe::from("s".repeat(300)));
        record.protocol = SafeId("");
        let line: Value = serde_json::to_value(&record).expect("serialize");
        assert_eq!(line["host"], REDACTED);
        assert_eq!(line["pool"], "farmbreak");
        assert_eq!(line["server"], "é");
        assert_eq!(line["servers"].as_str().map(str::len), Some(128));
        assert_eq!(line["protocol"], "unknown");
    }

    #[test]
    fn invalid_request_ids_get_a_fresh_correlation_id() {
        for supplied in [None, Some("bad id"), Some("")] {
            let mut record = http_access();
            record.correlation_id = Correlation(supplied);
            let line: Value = serde_json::to_value(&record).expect("serialize");
            let id = line["correlationId"].as_str().expect("correlation id");
            assert!(id.starts_with("op-") && valid_correlation_id(id), "{id}");
        }
    }

    #[test]
    fn http3_and_stuck_lines_carry_only_their_fields() {
        let http3 = Http3Access {
            correlation_id: Correlation(None),
            duration_ms: Count(4),
            host: Some(Safe::from("app.example.com")),
            method: Safe::from("GET"),
            protocol: SafeId("h3"),
            service: Safe::from("edge"),
            status: None,
            timestamp_unix_ms: Some(7),
        };
        let line: Value = serde_json::to_value(&http3).expect("serialize");
        let keys: Vec<_> = line
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "correlationId",
                "durationMs",
                "host",
                "method",
                "protocol",
                "service",
                "status",
                "timestampUnixMs"
            ]
        );
        assert_eq!(line["status"], Value::Null);

        let stuck = StuckAccess {
            age_ms: Count(61_000),
            correlation_id: Correlation(Some("lost-1")),
            event: SafeId("stuck"),
            phase: Safe::from("awaiting_headers"),
            pool: Some(Safe::from("farm")),
            request_id: Some(Safe::from("lost-1")),
            route: None,
            server: None,
            service: Safe::from("edge"),
            timestamp_unix_ms: 9,
        };
        assert_eq!(
            serde_json::to_string(&stuck).expect("serialize"),
            concat!(
                r#"{"ageMs":"61000","correlationId":"lost-1","event":"stuck","phase":"awaiting_headers","#,
                r#""pool":"farm","requestId":"lost-1","route":null,"server":null,"service":"edge","#,
                r#""timestampUnixMs":9}"#,
            )
        );
    }
}
