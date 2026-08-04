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

#[cfg(test)]
mod tests {
    use super::{redact_identifier, redact_text, REDACTED};

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
}
