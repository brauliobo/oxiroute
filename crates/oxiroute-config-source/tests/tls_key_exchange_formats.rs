use std::path::Path;

use oxiroute_config::{ConfigDraft, ConfigError, TlsKeyExchangeGroup, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::{Value, json};

const FORMATS: [ConfigFormat; 4] = [
    ConfigFormat::Kdl,
    ConfigFormat::Lua,
    ConfigFormat::Uci,
    ConfigFormat::Hocon,
];

fn draft(min_version: &str, policy: Value) -> Result<ValidatedConfig, serde_json::Error> {
    let draft = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "certificates": [{
            "name": "server",
            "dns_names": ["server.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/etc/oxiroute/server-chain.pem",
                "private_key_path": "/etc/oxiroute/server-key.pem"
            }
        }],
        "tls_profiles": [{
            "name": "web",
            "certificates": ["server"],
            "default_certificate": "server",
            "min_version": min_version,
            "policy": policy
        }],
        "listeners": []
    }))?;
    Ok(draft.validate().expect("valid TLS profile"))
}

fn groups(config: &ValidatedConfig) -> Option<Vec<TlsKeyExchangeGroup>> {
    config.as_draft().tls_profiles[0]
        .policy
        .key_exchange_groups
        .clone()
}

#[test]
fn key_exchange_groups_round_trip_through_every_source_format() {
    let config = draft(
        "1.2",
        json!({"key_exchange_groups": ["x25519", "secp256r1"]}),
    )
    .unwrap();
    assert_eq!(
        groups(&config),
        Some(vec![
            TlsKeyExchangeGroup::X25519,
            TlsKeyExchangeGroup::Secp256r1
        ])
    );
    for format in FORMATS {
        let rendered = render_config(format, &config).expect("render");
        assert!(
            rendered.contains("key_exchange_groups"),
            "{format:?}: {rendered}"
        );
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|error| panic!("{format:?}: {error}"));
        assert_eq!(resolved.config, config, "{format:?}");
    }
}

#[test]
fn unset_key_exchange_groups_keep_the_runtime_default_and_render_nothing() {
    let config = draft("1.2", json!({})).unwrap();
    assert_eq!(groups(&config), None);
    for format in FORMATS {
        let rendered = render_config(format, &config).expect("render");
        assert!(!rendered.contains("key_exchange_groups"), "{format:?}");
    }
}

#[test]
fn every_group_name_is_accepted() {
    let all = json!(["x25519mlkem768", "x25519", "secp256r1", "secp384r1"]);
    let config = draft("1.3", json!({"key_exchange_groups": all})).unwrap();
    assert_eq!(groups(&config).map(|groups| groups.len()), Some(4));
}

#[test]
fn unknown_group_names_are_rejected_by_the_decoder() {
    for name in ["P-256", "prime256v1", "ffdhe2048", "x448", "secp521r1", ""] {
        assert!(
            draft("1.2", json!({"key_exchange_groups": [name]})).is_err(),
            "{name:?}"
        );
    }
}

fn validation_error(min_version: &str, groups: Value) -> ConfigError {
    let draft = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "certificates": [{
            "name": "server",
            "dns_names": ["server.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/etc/oxiroute/server-chain.pem",
                "private_key_path": "/etc/oxiroute/server-key.pem"
            }
        }],
        "tls_profiles": [{
            "name": "web",
            "certificates": ["server"],
            "default_certificate": "server",
            "min_version": min_version,
            "policy": {"key_exchange_groups": groups}
        }],
        "listeners": []
    }))
    .expect("authored profile");
    draft.validate().expect_err("invalid key exchange groups")
}

#[test]
fn empty_and_repeated_group_lists_are_rejected() {
    for list in [json!([]), json!(["x25519", "x25519"])] {
        assert!(matches!(
            validation_error("1.2", list),
            ConfigError::InvalidTlsProfilePolicy {
                field: "key_exchange_groups",
                ..
            }
        ));
    }
}

#[test]
fn hybrid_only_lists_need_tls_13_because_tls_12_has_no_hybrid_groups() {
    assert!(matches!(
        validation_error("1.2", json!(["x25519mlkem768"])),
        ConfigError::InvalidTlsProfilePolicy {
            field: "key_exchange_groups",
            ..
        }
    ));
    draft("1.3", json!({"key_exchange_groups": ["x25519mlkem768"]})).unwrap();
    draft(
        "1.2",
        json!({"key_exchange_groups": ["x25519mlkem768", "x25519"]}),
    )
    .unwrap();
}
