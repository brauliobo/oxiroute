use oxiroute_config::{ConfigCompositionError, ConfigDraft, ConfigError, ValidatedConfig};
use oxiroute_config_source::compose_validated_fragments;
use serde_json::{Value, json};

fn draft(value: Value) -> ConfigDraft {
    serde_json::from_value(value).expect("configuration draft")
}

fn validated(value: Value) -> ValidatedConfig {
    draft(value).validate().expect("validated fragment")
}

#[test]
fn rejects_duplicate_names_after_validated_namespaces_are_merged() {
    let first = validated(json!({
        "version": 1,
        "listeners": [],
        "cache_stores": [{"name": "shared", "type": "memory"}]
    }));
    let second = validated(json!({
        "version": 1,
        "listeners": [],
        "cache_stores": [{"name": "shared", "type": "memory"}]
    }));

    assert!(matches!(
        compose_validated_fragments(None, vec![first, second], 1),
        Err(ConfigCompositionError::Invalid(ConfigError::DuplicateName {
            namespace: "cache store",
            name,
        })) if name == "shared"
    ));
}

#[test]
fn rejects_process_field_conflicts_between_validated_fragments() {
    let first = validated(json!({
        "version": 1,
        "max_connections": 1024,
        "listeners": []
    }));
    let second = validated(json!({
        "version": 1,
        "max_connections": 4096,
        "listeners": []
    }));

    assert!(matches!(
        compose_validated_fragments(None, vec![first, second], 1),
        Err(ConfigCompositionError::ProcessFieldConflict {
            field: "max_connections"
        })
    ));
}

#[test]
fn validates_cross_fragment_references_only_after_complete_composition() {
    let authored = draft(json!({
        "version": 1,
        "listeners": [{
            "name": "edge",
            "bind": {"type": "socket", "address": "127.0.0.1:18080"},
            "protocol": "tcp",
            "service": "edge"
        }],
        "l4_services": [{"name": "edge", "upstream_pool": "shared"}]
    }));
    assert!(matches!(
        authored.clone().validate(),
        Err(ConfigError::UnknownL4UpstreamPool { .. })
    ));
    let imported = validated(json!({
        "version": 1,
        "listeners": [],
        "upstream_pools": [{
            "name": "shared",
            "servers": [{
                "name": "origin",
                "endpoint": {"type": "socket", "address": "127.0.0.1:19090"}
            }]
        }]
    }));

    let composed = compose_validated_fragments(Some(authored), vec![imported], 1)
        .expect("complete source composition");

    assert_eq!(composed.as_draft().listeners.len(), 1);
    assert_eq!(composed.as_draft().upstream_pools.len(), 1);
    assert_eq!(composed.as_draft().l4_services[0].upstream_pool, "shared");
}
