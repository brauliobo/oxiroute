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

#[test]
fn openssl_is_the_default_backend() {
    assert_eq!(backend(&json!({})), TlsBackend::Openssl);
    assert_eq!(backend(&json!({"backend": "openssl"})), TlsBackend::Openssl);
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
fn rustls_rejects_the_fields_only_openssl_can_honour() {
    for (field, value) in [
        ("cipher_list", json!("ECDHE-ECDSA-AES128-GCM-SHA256")),
        ("dh_parameters_path", json!("/etc/oxiroute/dhparam.pem")),
    ] {
        let mut policy = json!({"backend": "rustls"});
        policy[field] = value.clone();

        let error = validate(&policy).unwrap_err();
        assert!(
            matches!(&error, ConfigError::InvalidTlsProfilePolicy { field: rejected, .. } if *rejected == field),
            "{field}: {error}"
        );

        policy["backend"] = json!("openssl");
        validate(&policy).unwrap_or_else(|error| panic!("{field} on openssl: {error}"));
    }
}
