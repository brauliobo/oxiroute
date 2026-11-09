//! Direct JSON rendering of the per-request access line.
//!
//! The derived `Serialize` form is the specification; this renders the same bytes without the serializer: keys are
//! pre-escaped literals, numbers are written with a small decimal loop and text is copied once when nothing needs
//! escaping. Anything that needs an escape goes through `serde_json`, so the output cannot diverge.

use super::{Correlation, Count, HttpAccess, Safe, SafeId};
use crate::observability::logging::{CorrelationId, redact_text_cow, valid_correlation_id};

trait Json {
    fn write_json(&self, out: &mut Vec<u8>);
}

fn push_u64(out: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0_u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[start..]);
}

fn push_decimal(out: &mut Vec<u8>, value: u128) {
    match u64::try_from(value) {
        Ok(value) => push_u64(out, value),
        Err(_) => out.extend_from_slice(value.to_string().as_bytes()),
    }
}

fn push_text(out: &mut Vec<u8>, text: &str) {
    if text
        .bytes()
        .any(|byte| byte < 0x20 || byte == b'"' || byte == b'\\')
    {
        serde_json::to_writer(&mut *out, text).expect("writing to a vec cannot fail");
    } else {
        out.push(b'"');
        out.extend_from_slice(text.as_bytes());
        out.push(b'"');
    }
}

impl Json for usize {
    fn write_json(&self, out: &mut Vec<u8>) {
        push_u64(out, *self as u64);
    }
}

impl Json for u16 {
    fn write_json(&self, out: &mut Vec<u8>) {
        push_u64(out, u64::from(*self));
    }
}

impl Json for u64 {
    fn write_json(&self, out: &mut Vec<u8>) {
        push_u64(out, *self);
    }
}

impl Json for bool {
    fn write_json(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(if *self { b"true" } else { b"false" });
    }
}

impl Json for Count<u64> {
    fn write_json(&self, out: &mut Vec<u8>) {
        out.push(b'"');
        push_u64(out, self.0);
        out.push(b'"');
    }
}

impl Json for Count<u128> {
    fn write_json(&self, out: &mut Vec<u8>) {
        out.push(b'"');
        push_decimal(out, self.0);
        out.push(b'"');
    }
}

impl Json for Safe<'_> {
    fn write_json(&self, out: &mut Vec<u8>) {
        push_text(out, &redact_text_cow(&self.0));
    }
}

impl Json for SafeId<'_> {
    fn write_json(&self, out: &mut Vec<u8>) {
        match redact_text_cow(self.0) {
            text if text.is_empty() => push_text(out, "unknown"),
            text => push_text(out, &text),
        }
    }
}

impl Json for Correlation<'_> {
    fn write_json(&self, out: &mut Vec<u8>) {
        match self.0.filter(|id| valid_correlation_id(id)) {
            Some(id) => push_text(out, id),
            None => push_text(out, &CorrelationId::next().to_string()),
        }
    }
}

impl<T: Json> Json for Option<T> {
    fn write_json(&self, out: &mut Vec<u8>) {
        match self {
            Some(value) => value.write_json(out),
            None => out.extend_from_slice(b"null"),
        }
    }
}

/// `prefix` carries the separator and the escaped key, e.g. `,"bytesSent":`.
fn field(out: &mut Vec<u8>, prefix: &[u8], value: &impl Json) {
    out.extend_from_slice(prefix);
    value.write_json(out);
}

impl HttpAccess<'_> {
    /// Appends the line without the trailing newline; keys are in the order of the derived `Serialize` form.
    pub(crate) fn write_json(&self, out: &mut Vec<u8>) {
        field(out, br#"{"attempts":"#, &self.attempts);
        field(out, br#","bytesReceived":"#, &self.bytes_received);
        field(out, br#","bytesSent":"#, &self.bytes_sent);
        field(out, br#","connectMs":"#, &self.connect_ms);
        field(out, br#","correlationId":"#, &self.correlation_id);
        field(out, br#","durationMs":"#, &self.duration_ms);
        field(out, br#","evacuations":"#, &self.evacuations);
        field(out, br#","headerMs":"#, &self.header_ms);
        field(out, br#","host":"#, &self.host);
        field(out, br#","idemKey":"#, &self.idem_key);
        field(out, br#","method":"#, &self.method);
        field(out, br#","pool":"#, &self.pool);
        field(out, br#","protocol":"#, &self.protocol);
        field(out, br#","queueWaitMs":"#, &self.queue_wait_ms);
        field(out, br#","reason":"#, &self.reason);
        field(out, br#","requestId":"#, &self.request_id);
        field(out, br#","rescued":"#, &self.rescued);
        field(out, br#","route":"#, &self.route);
        field(out, br#","server":"#, &self.server);
        field(out, br#","servers":"#, &self.servers);
        field(out, br#","service":"#, &self.service);
        field(out, br#","status":"#, &self.status);
        field(out, br#","timestampUnixMs":"#, &self.timestamp_unix_ms);
        field(out, br#","wasStuck":"#, &self.was_stuck);
        out.push(b'}');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(record: &HttpAccess<'_>) -> String {
        let mut out = Vec::new();
        record.write_json(&mut out);
        String::from_utf8(out).expect("utf8")
    }

    fn base() -> HttpAccess<'static> {
        HttpAccess {
            attempts: 0,
            bytes_received: Count(0),
            bytes_sent: Count(u64::MAX),
            connect_ms: None,
            correlation_id: Correlation(Some("req-1")),
            duration_ms: Count(u128::from(u64::MAX) + 1),
            evacuations: 3,
            header_ms: Some(Count(10)),
            host: Some(Safe::from("app.example.com")),
            idem_key: Some(Safe::from("1a2b3c4d")),
            method: Safe::from("GET"),
            pool: None,
            protocol: SafeId("h2"),
            queue_wait_ms: Count(12_345),
            reason: Safe::from("upstream_timeout"),
            request_id: Some(Safe::from("req-1")),
            rescued: true,
            route: Some(Safe::from("api")),
            server: Some(Safe::from("gpu-2")),
            servers: Some(Safe::from("gpu-1,gpu-2")),
            service: Safe::from("edge"),
            status: Some(503),
            timestamp_unix_ms: Some(1_700_000_000_000),
            was_stuck: false,
        }
    }

    #[test]
    fn the_direct_line_equals_the_serialized_line() {
        let mut escaped = base();
        escaped.host = Some(Safe::from("quote\" back\\slash tab\t é"));
        escaped.reason = Safe::from("Authorization: Bearer abc");
        escaped.servers = Some(Safe::from("s".repeat(300)));
        escaped.protocol = SafeId("");
        let mut empty = base();
        empty.status = None;
        empty.timestamp_unix_ms = None;
        empty.host = None;
        for record in [base(), escaped, empty] {
            assert_eq!(
                rendered(&record),
                serde_json::to_string(&record).expect("serialize")
            );
        }
    }

    #[test]
    fn an_invalid_request_id_is_replaced_by_a_valid_correlation_id() {
        let mut record = base();
        record.correlation_id = Correlation(Some("bad id"));
        let line: serde_json::Value = serde_json::from_str(&rendered(&record)).expect("json");
        let id = line["correlationId"].as_str().expect("correlation id");
        assert!(id.starts_with("op-") && valid_correlation_id(id), "{id}");
    }
}
