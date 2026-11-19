use oxiroute_config::{ClientTracking, ClientTrackingSettings, ConfigDraft, ConfigError};
use serde_json::{Value, json};

fn validate(tracking: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "client_tracking": tracking}),
    )
    .expect("authored configuration")
    .validate()
}

fn rejected(tracking: &Value) -> (String, String) {
    match validate(tracking).unwrap_err() {
        ConfigError::InvalidClientTracking { pointer, detail } => (pointer.to_owned(), detail),
        error => panic!("unexpected error: {error}"),
    }
}

#[test]
fn tracking_is_on_with_the_defaults_when_the_block_is_absent() {
    let config: ConfigDraft =
        serde_json::from_value(json!({"version": 1, "listeners": []})).unwrap();
    assert_eq!(config.client_tracking, None);
    assert_eq!(
        config.client_tracking(),
        ClientTrackingSettings {
            enabled: true,
            max_clients: 10_000,
            ipv6_prefix: 128,
        }
    );
    let canonical = serde_json::to_value(config.validate().unwrap()).unwrap();
    assert!(canonical.get("client_tracking").is_none());
}

#[test]
fn an_empty_block_normalizes_away() {
    let config = validate(&json!({})).unwrap();
    assert_eq!(config.as_draft().client_tracking, None);
    assert!(config.as_draft().client_tracking().enabled);
}

#[test]
fn set_values_override_the_defaults_one_by_one() {
    let config = validate(&json!({"enabled": false})).unwrap();
    assert_eq!(
        config.as_draft().client_tracking(),
        ClientTrackingSettings {
            enabled: false,
            max_clients: 10_000,
            ipv6_prefix: 128,
        }
    );
    let config = validate(&json!({"max_clients": 25, "ipv6_prefix": 64})).unwrap();
    assert_eq!(
        config.as_draft().client_tracking(),
        ClientTrackingSettings {
            enabled: true,
            max_clients: 25,
            ipv6_prefix: 64,
        }
    );
}

#[test]
fn the_limits_are_accepted() {
    for (clients, prefix) in [
        (ClientTracking::MIN_MAX_CLIENTS, 64),
        (ClientTracking::MAX_MAX_CLIENTS, 128),
    ] {
        let config = validate(&json!({"max_clients": clients, "ipv6_prefix": prefix})).unwrap();
        let tracking = config.as_draft().client_tracking.clone().unwrap();
        assert_eq!(tracking.max_clients, Some(clients));
        assert_eq!(tracking.ipv6_prefix, Some(prefix));
    }
}

#[test]
fn rejected_values_name_the_setting_and_the_allowed_values() {
    for (tracking, pointer, allowed) in [
        (
            json!({"max_clients": 0}),
            "/client_tracking/max_clients",
            "between 1 and 1000000",
        ),
        (
            json!({"max_clients": 1_000_001}),
            "/client_tracking/max_clients",
            "between 1 and 1000000",
        ),
        (
            json!({"ipv6_prefix": 48}),
            "/client_tracking/ipv6_prefix",
            "64 or 128",
        ),
        (
            json!({"ipv6_prefix": 0}),
            "/client_tracking/ipv6_prefix",
            "64 or 128",
        ),
    ] {
        let (found, detail) = rejected(&tracking);
        assert_eq!(found, pointer);
        assert!(detail.contains(allowed), "{detail}");
    }
}

#[test]
fn wrongly_typed_or_unknown_settings_are_refused() {
    for tracking in [
        json!({"ipv6_prefix": 256}),
        json!({"max_clients": -1}),
        json!({"enabled": "yes"}),
        json!({"ttl": 3}),
    ] {
        let result = serde_json::from_value::<ConfigDraft>(
            json!({"version": 1, "listeners": [], "client_tracking": tracking}),
        );
        assert!(result.is_err(), "{tracking}");
    }
}

#[test]
fn the_error_points_at_the_setting() {
    let error = validate(&json!({"ipv6_prefix": 48})).unwrap_err();
    assert_eq!(
        error.pointer().as_deref(),
        Some("/client_tracking/ipv6_prefix")
    );
}
