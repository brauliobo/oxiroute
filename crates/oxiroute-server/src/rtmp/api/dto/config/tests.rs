use super::*;
use oxiroute_config::{
    RtmpApplication, RtmpAutoPushPolicy, RtmpCallbackConfig, RtmpFanoutPolicy, RtmpOutboundPolicy,
    RtmpRelayPolicy, RtmpService, RtmpSessionCeilings, RtmpTokenPolicy, RtmpTokenSource,
};
use serde_json::json;

fn config() -> ConfigDraft {
    serde_json::from_value(json!({ "version": 1, "listeners": [] })).expect("minimal configuration")
}

fn config_with_token(secret: &str) -> ConfigDraft {
    let mut config = config();
    config.rtmp_services.push(RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        max_inbound_message_size: 8 * 1_024 * 1_024,
        ack_window_size: 5_000_000,
        access_log: None,
        outbound_policy: RtmpOutboundPolicy::default(),
        callbacks: RtmpCallbackConfig::default(),
        auto_push: RtmpAutoPushPolicy::default(),
        exec_profiles: Vec::new(),
        applications: vec![RtmpApplication {
            name: "broadcast".into(),
            live: true,
            idle_streams: true,
            publish: RtmpAccessPolicy {
                rules: Vec::new(),
                token: Some(RtmpTokenPolicy {
                    source: RtmpTokenSource::StreamQuery,
                    parameter: "token".into(),
                    secret: secret.into(),
                }),
            },
            play: RtmpAccessPolicy::default(),
            limits: RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: RtmpRelayPolicy::default(),
            callbacks: RtmpCallbackConfig::default(),
            fanout: RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: Vec::new(),
        }],
    });
    config
}

#[test]
fn request_is_strict_and_distinct_from_the_redacted_view() {
    let request: ConfigRequest = serde_json::from_value(json!({
        "config": { "version": 1, "listeners": [] }
    }))
    .expect("typed request");
    assert_eq!(request.into_config(), config());
    assert!(
        serde_json::from_value::<ConfigRequest>(json!({
            "config": { "version": 1, "listeners": [] },
            "normalizedConfig": { "version": 1, "listeners": [] }
        }))
        .is_err()
    );
}

#[test]
fn redacted_view_preserves_the_sentinel_and_lua_alias_without_the_secret() {
    let validated = config_with_token("private-token").validate().unwrap();
    let (redacted, preview, lua_preview) =
        RedactedConfigView::new(&validated, ConfigFormat::Lua).into_parts();
    let redacted = serde_json::to_value(redacted).unwrap();

    assert_eq!(
        redacted["rtmp_services"][0]["applications"][0]["publish"]["token"]["secret"],
        "<redacted>"
    );
    assert_eq!(lua_preview.as_deref(), Some(preview.as_str()));
    assert!(!preview.contains("private-token"));
}

#[test]
fn sentinel_merge_restores_only_the_matching_authoritative_token() {
    let authoritative = config_with_token("private-token");
    let mut round_trip = config_with_token("<redacted>");

    assert!(contains_redacted_rtmp_token_secret(&round_trip));
    restore_redacted_rtmp_token_secrets(&mut round_trip, &authoritative);
    assert_eq!(
        round_trip.rtmp_services[0].applications[0]
            .publish
            .token
            .as_ref()
            .unwrap()
            .secret,
        "private-token"
    );

    round_trip.rtmp_services[0].applications[0]
        .publish
        .token
        .as_mut()
        .unwrap()
        .secret = "replacement-token".into();
    restore_redacted_rtmp_token_secrets(&mut round_trip, &authoritative);
    assert_eq!(
        round_trip.rtmp_services[0].applications[0]
            .publish
            .token
            .as_ref()
            .unwrap()
            .secret,
        "replacement-token"
    );
}

#[test]
fn snapshot_dto_has_the_exact_top_level_json_contract() {
    let validated = config().validate().unwrap();
    let document = ResolvedConfigDocument {
        authored_revision: AuthoredRevision::from_bytes(b"disk"),
        effective_revision: EffectiveRevision::from_bytes(b"candidate"),
        validated_config: validated.clone(),
        format: ConfigFormat::Kdl,
        compositional: false,
        dependencies: Vec::new(),
        config_preview: "ignored unredacted preview".into(),
        diagnostics: Vec::new(),
    };
    let response = ConfigSnapshotResponse::new(
        &document,
        EffectiveRevision::from_bytes(b"active"),
        RedactedConfigView::new(&validated, ConfigFormat::Kdl),
    );
    let value = serde_json::to_value(response).unwrap();

    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "activeRevision",
            "candidateRevision",
            "compositional",
            "config",
            "configFormat",
            "configPreview",
            "dependencyCount",
            "diagnostics",
            "diskRevision",
            "schemaVersion",
        ]
    );
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["config"], serde_json::to_value(validated).unwrap());
    assert!(value.get("luaPreview").is_none());
}
