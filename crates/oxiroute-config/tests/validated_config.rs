use oxiroute_config::{ConfigDraft, ConfigError};
use serde_json::json;

fn normalizable_config() -> ConfigDraft {
    serde_json::from_value(json!({
        "version": 1,
        "certificates": [{
            "name": "local",
            "dns_names": ["LOCALHOST"],
            "source": {"type": "self_signed_development", "validity_days": 7}
        }],
        "listeners": []
    }))
    .expect("normalizable configuration")
}

#[test]
fn owned_validation_is_idempotent_and_serializes_as_the_inner_config() {
    let validated = normalizable_config()
        .validate()
        .expect("valid configuration");
    let revalidated = validated
        .to_draft()
        .validate()
        .expect("revalidated configuration");

    assert_eq!(revalidated, validated);
    assert_eq!(
        validated.as_draft().certificates[0].dns_names,
        ["localhost"]
    );
    assert_eq!(
        serde_json::to_value(&validated).expect("validated JSON"),
        serde_json::to_value(validated.as_draft()).expect("draft JSON")
    );
}

#[test]
fn failed_owned_validation_returns_no_partially_normalized_config() {
    let authored: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "certificates": [{
            "name": "local",
            "dns_names": ["LOCALHOST"],
            "source": {"type": "self_signed_development", "validity_days": 7}
        }],
        "listeners": [{
            "name": "broken",
            "bind": {"type": "socket", "address": "127.0.0.1:8080"},
            "protocol": "tcp",
            "service": "missing"
        }]
    }))
    .expect("invalid authored configuration");

    assert!(matches!(
        authored.clone().validate(),
        Err(ConfigError::UnknownListenerService { .. })
    ));
    assert_eq!(authored.certificates[0].dns_names, ["LOCALHOST"]);
}
