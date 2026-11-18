use super::*;

#[test]
fn fingerprint_is_a_short_stable_sha256_prefix() {
    assert_eq!(secret_fingerprint("a"), "fp-ca978112");
    assert_eq!(
        secret_fingerprint("demo-secret-not-real"),
        secret_fingerprint("demo-secret-not-real")
    );
    assert_ne!(
        secret_fingerprint("demo-secret-not-real"),
        secret_fingerprint("another-secret-not-real")
    );
}

#[test]
fn conventional_and_policy_parameters_are_replaced_by_fingerprints() {
    let redacted = redact_query(
        "token=demo-secret-not-real&quality=hd&Key=k1&viewer=v1&%74oken=percent-encoded",
        &["viewer"],
    );
    assert!(redacted.contains("quality=hd"));
    assert!(!redacted.contains("demo-secret-not-real"));
    assert!(!redacted.contains("k1"));
    assert!(!redacted.contains("v1"));
    assert!(!redacted.contains("percent-encoded"));
    assert_eq!(redacted.matches("=fp-").count(), 4);
}

#[test]
fn a_query_without_secrets_is_unchanged() {
    assert_eq!(redact_query("quality=hd&flag", &[]), "quality=hd&flag");
}
