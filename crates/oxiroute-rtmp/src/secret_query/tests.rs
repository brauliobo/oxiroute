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
fn listed_fingerprints_sort_by_service_application_then_role_name() {
    let entry = |service: &str, application: &str, role| RtmpSecretFingerprint {
        service: service.into(),
        application: application.into(),
        role,
        fingerprint: secret_fingerprint("a"),
    };
    let mut entries = [
        entry("live", "cam", RtmpSecretRole::Publish),
        entry("live", "cam", RtmpSecretRole::Play),
        entry("live", "bay", RtmpSecretRole::Publish),
        entry("edge", "zed", RtmpSecretRole::Play),
    ];
    entries.sort();
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                entry.service.as_str(),
                entry.application.as_str(),
                entry.role
            ))
            .collect::<Vec<_>>(),
        [
            ("edge", "zed", RtmpSecretRole::Play),
            ("live", "bay", RtmpSecretRole::Publish),
            ("live", "cam", RtmpSecretRole::Play),
            ("live", "cam", RtmpSecretRole::Publish),
        ]
    );
}

#[test]
fn a_query_without_secrets_is_unchanged() {
    assert_eq!(redact_query("quality=hd&flag", &[]), "quality=hd&flag");
}
