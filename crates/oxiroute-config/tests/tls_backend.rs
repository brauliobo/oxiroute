use oxiroute_config::{ConfigDraft, ConfigError, TlsBackend};
use serde_json::{Value, json};

fn validate(policy: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "certificates": [{
            "name": "dev",
            "dns_names": ["www.example.test"],
            "source": {"type": "self_signed_development"},
        }],
        "tls_profiles": [{
            "name": "edge",
            "certificates": ["dev"],
            "default_certificate": "dev",
            "policy": policy,
        }],
    }))
    .expect("authored configuration")
    .validate()
}

fn backend(policy: &Value) -> TlsBackend {
    validate(policy).unwrap().as_draft().tls_profiles[0]
        .policy
        .backend
}

/// The backend of a profile that does not name one. Moving the default is this constant and the `#[default]` variant
/// of `TlsBackend`; every other test takes the default from `TlsBackend::default()`.
const DEFAULT_BACKEND: TlsBackend = TlsBackend::Openssl;

#[test]
fn a_profile_without_a_backend_gets_the_default_and_both_can_be_named() {
    assert_eq!(TlsBackend::default(), DEFAULT_BACKEND);
    assert_eq!(backend(&json!({})), DEFAULT_BACKEND);
    assert_eq!(backend(&json!({"backend": "openssl"})), TlsBackend::Openssl);
    assert_eq!(backend(&json!({"backend": "rustls"})), TlsBackend::Rustls);
}

#[test]
fn rustls_is_taken_as_written_and_stays_in_the_canonical_form() {
    let config = validate(&json!({"backend": "rustls"})).unwrap();

    assert_eq!(
        config.as_draft().tls_profiles[0].policy.backend,
        TlsBackend::Rustls
    );
    let canonical = serde_json::to_value(&config).unwrap();
    assert_eq!(canonical["tls_profiles"][0]["policy"]["backend"], "rustls");
}

#[test]
fn an_unknown_backend_is_rejected() {
    let draft = json!({"version": 1, "listeners": [], "tls_profiles": [{
        "name": "edge", "certificates": [], "default_certificate": "dev", "policy": {"backend": "boringssl"},
    }]});
    assert!(serde_json::from_value::<ConfigDraft>(draft).is_err());
}

#[test]
fn rustls_takes_the_cipher_list_and_the_dh_parameters_like_openssl() {
    for (field, value) in [
        ("cipher_list", json!("ECDHE-ECDSA-AES128-GCM-SHA256:HIGH")),
        ("dh_parameters_path", json!("/etc/oxiroute/dhparam.pem")),
    ] {
        for backend in ["openssl", "rustls"] {
            let mut policy = json!({"backend": backend});
            policy[field] = value.clone();

            let config =
                validate(&policy).unwrap_or_else(|error| panic!("{field} on {backend}: {error}"));
            let canonical = serde_json::to_value(&config).unwrap();
            assert_eq!(
                canonical["tls_profiles"][0]["policy"][field], value,
                "{field} on {backend}"
            );
        }
    }
}
