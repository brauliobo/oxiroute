use std::{collections::BTreeSet, str::FromStr};

use oxiroute_rtmp::{
    EndedStreamSnapshot, MediaSnapshot, PublisherEndReason, RtmpCapabilities, RtmpCatalogSnapshot,
    RtmpSessionControlAction, RtmpSessionRole, SessionId, StreamId, StreamKey, StreamProfile,
    StreamSnapshot, TrackSnapshot,
};
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use super::*;

fn schema<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12().into_generator();
    serde_json::to_value(generator.into_root_schema_for::<T>()).expect("response schema")
}

fn empty_catalog() -> RtmpCatalogSnapshot {
    RtmpCatalogSnapshot {
        revision: 7,
        as_of_unix_ms: 1_750_000_000_000,
        capabilities: RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        },
        streams: Vec::new(),
        ended_streams: Vec::new(),
    }
}

#[test]
fn empty_stats_and_catalog_match_the_existing_v1_golden_objects() {
    let catalog = empty_catalog();
    assert_eq!(
        serde_json::to_value(RtmpStatsResponse::project(&catalog, &[], 1_024, 1_024))
            .expect("stats JSON"),
        json!({
            "revision": "7",
            "asOfUnixMs": 1_750_000_000_000_u64,
            "global": {
                "activeStreams": 0,
                "publishers": 0,
                "subscribers": 0,
                "audioPayloadBytes": "0",
                "videoPayloadBytes": "0",
                "liveIngest": true,
                "manualRecording": false,
            },
            "live": [],
            "clients": [],
            "liveTruncated": false,
            "clientsTruncated": false,
        })
    );
    assert_eq!(
        serde_json::to_value(RtmpCatalogResponse::project(&catalog)).expect("catalog JSON"),
        json!({
            "revision": "7",
            "as_of_unix_ms": 1_750_000_000_000_u64,
            "capabilities": { "live_ingest": true, "manual_recording": false },
            "streams": [],
            "recently_ended_streams": [],
        })
    );
}

#[test]
fn session_control_success_and_conflicts_match_the_v1_golden_objects() {
    let session_id = SessionId::new();
    assert_eq!(
        serde_json::to_value(RtmpSessionControlResponse::project(
            false,
            &session_id,
            RtmpSessionControlAction::Publisher,
            u64::MAX,
        ))
        .expect("control response JSON"),
        json!({
            "outcome": "requested",
            "sessionId": session_id.to_string(),
            "target": "publisher",
            "sessionRevision": u64::MAX.to_string(),
        })
    );
    assert_eq!(
        serde_json::to_value(RtmpSessionRevisionConflictResponse::new(9))
            .expect("revision conflict JSON"),
        json!({
            "error": {
                "code": "session_revision_conflict",
                "message": "RTMP session revision changed",
            },
            "actualRevision": 9,
        })
    );
    assert_eq!(
        serde_json::to_value(RtmpSessionRoleConflictResponse::new(
            RtmpSessionRole::Subscriber,
        ))
        .expect("role conflict JSON"),
        json!({
            "error": {
                "code": "session_role_conflict",
                "message": "RTMP session role does not match the requested target",
            },
            "actualRole": "subscriber",
        })
    );
}

#[test]
fn stats_and_catalog_preserve_maximum_cumulative_counters_as_decimal_strings() {
    let mut catalog = empty_catalog();
    catalog.streams.push(StreamSnapshot {
        id: StreamId::from_str("2a130dea-5db7-43e0-afb8-f07c4bcb1814").expect("fixed stream ID"),
        revision: u64::MAX,
        key: StreamKey::new("edge", "live", "camera"),
        created_at_unix_ms: 1_750_000_000_000,
        publisher: None,
        subscriber_count: 0,
        media: MediaSnapshot {
            audio: TrackSnapshot {
                payload_bytes_received: u64::MAX,
                ..TrackSnapshot::default()
            },
            video: TrackSnapshot::default(),
            profile: StreamProfile::default(),
            fanout_payload_bytes_queued: u64::MAX,
        },
        relays: Vec::new(),
        recorders: Vec::new(),
    });

    let stats =
        serde_json::to_value(RtmpStatsResponse::project(&catalog, &[], 1, 1)).expect("stats JSON");
    let catalog =
        serde_json::to_value(RtmpCatalogResponse::project(&catalog)).expect("catalog JSON");

    assert_eq!(stats["global"]["audioPayloadBytes"], u64::MAX.to_string());
    assert_eq!(stats["live"][0]["audioPayloadBytes"], u64::MAX.to_string());
    assert_eq!(catalog["streams"][0]["revision"], u64::MAX.to_string());
    assert_eq!(
        catalog["streams"][0]["media"]["audio"]["payload_bytes"],
        u64::MAX.to_string()
    );
    assert_eq!(
        catalog["streams"][0]["media"]["fanout_payload_bytes"],
        u64::MAX.to_string()
    );
}

#[test]
fn rtmp_response_schemas_are_closed_structural_projections_with_decimal_counters() {
    let schemas = [
        schema::<RtmpStatsResponse>(),
        schema::<RtmpGlobalStatsResponse>(),
        schema::<RtmpLiveStatsResponse>(),
        schema::<RtmpClientStatsResponse>(),
        schema::<RtmpCatalogResponse>(),
        schema::<RtmpStreamResponse>(),
        schema::<RtmpRecorderResponse>(),
        schema::<RtmpSessionControlResponse>(),
        schema::<RtmpSessionRevisionConflictResponse>(),
        schema::<RtmpSessionRoleConflictResponse>(),
    ];
    for response in &schemas {
        let encoded = response.to_string();
        assert!(!encoded.contains("serde_json::Value"));
        assert!(!encoded.contains("\"additionalProperties\":true"));
    }
    let catalog = schema::<RtmpCatalogResponse>();
    assert_eq!(catalog["$defs"]["DecimalCounter"]["type"], "string");
    assert_eq!(
        catalog["$defs"]["RtmpRelayDto"]["properties"]["payload_bytes_sent"]["$ref"],
        "#/$defs/DecimalCounter"
    );
    assert_eq!(
        catalog["$defs"]["RtmpRecorderResponse"]["properties"]["bytes_written"]["$ref"],
        "#/$defs/DecimalCounter"
    );
}

#[test]
fn rtmp_schemas_own_the_expected_nested_sections_and_exclude_secret_fields() {
    let catalog_schema = schema::<RtmpCatalogResponse>();
    let definitions = catalog_schema["$defs"]
        .as_object()
        .expect("catalog schema definitions")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for expected in [
        "RtmpPublisherDto",
        "RtmpTrackDto",
        "RtmpRelayDto",
        "RtmpRecorderResponse",
    ] {
        assert!(definitions.contains(expected), "missing {expected}");
    }
    let schemas = [
        schema::<RtmpStatsResponse>(),
        schema::<RtmpCatalogResponse>(),
        schema::<RtmpSessionControlResponse>(),
    ]
    .into_iter()
    .map(|schema| schema.to_string().to_ascii_lowercase())
    .collect::<String>();
    for forbidden in ["token", "password", "secret", "authorization", "privatekey"] {
        assert!(!schemas.contains(forbidden), "schema exposed {forbidden}");
    }
}

#[test]
fn recently_ended_streams_are_offline_with_last_seen_end_reason_and_profile() {
    let mut catalog = empty_catalog();
    catalog.ended_streams.push(EndedStreamSnapshot {
        stream_id: StreamId::from_str("2a130dea-5db7-43e0-afb8-f07c4bcb1814")
            .expect("fixed stream ID"),
        key: StreamKey::new("edge", "live", "camera"),
        publisher_session_id: SessionId::from_str("5b9c6a52-4f55-4d62-9d4c-3a1f7a4e5c10")
            .expect("fixed session ID"),
        created_at_unix_ms: 1_000,
        published_at_unix_ms: 2_000,
        last_seen_at_unix_ms: 9_000,
        ended_at_unix_ms: 9_500,
        end_reason: PublisherEndReason::Stale,
        media: MediaSnapshot {
            profile: StreamProfile {
                video_width: Some(1280),
                video_height: Some(720),
                frame_rate_millihertz: Some(30_000),
                keyframe_interval_ms: Some(2_000),
                ..StreamProfile::default()
            },
            ..MediaSnapshot::default()
        },
    });

    let catalog =
        serde_json::to_value(RtmpCatalogResponse::project(&catalog)).expect("catalog JSON");
    let ended = &catalog["recently_ended_streams"][0];

    assert_eq!(ended["state"], "offline");
    assert_eq!(ended["last_seen_at_unix_ms"], 9_000);
    assert_eq!(ended["ended_at_unix_ms"], 9_500);
    assert_eq!(ended["end_reason"], "stale");
    assert_eq!(ended["media"]["profile"]["video_width"], 1280);
    assert_eq!(ended["media"]["profile"]["frame_rate_millihertz"], 30_000);
    assert_eq!(ended["media"]["profile"]["keyframe_interval_ms"], 2_000);
}
