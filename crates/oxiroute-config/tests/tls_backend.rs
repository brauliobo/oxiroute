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

fn backend(policy: &Value) -> Option<TlsBackend> {
    validate(policy).unwrap().as_draft().tls_profiles[0]
        .policy
        .backend
}

#[test]
fn a_profile_without_a_backend_leaves_the_choice_to_planning_and_both_can_be_named() {
    assert_eq!(TlsBackend::default(), TlsBackend::Rustls);
    assert_eq!(backend(&json!({})), None);
    assert_eq!(
        backend(&json!({"backend": "openssl"})),
        Some(TlsBackend::Openssl)
    );
    assert_eq!(
        backend(&json!({"backend": "rustls"})),
        Some(TlsBackend::Rustls)
    );
}

#[test]
fn an_omitted_backend_stays_omitted_in_the_canonical_form() {
    let canonical = serde_json::to_value(validate(&json!({})).unwrap()).unwrap();

    assert!(
        canonical["tls_profiles"][0]["policy"]
            .get("backend")
            .is_none()
    );
}

#[test]
fn rustls_is_taken_as_written_and_stays_in_the_canonical_form() {
    let config = validate(&json!({"backend": "rustls"})).unwrap();

    assert_eq!(
        config.as_draft().tls_profiles[0].policy.backend,
        Some(TlsBackend::Rustls)
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
fn the_cipher_list_and_the_dh_parameters_are_kept_whatever_the_backend() {
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
