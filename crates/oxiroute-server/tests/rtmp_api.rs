use std::sync::Arc;

use oxiroute_rtmp::{
    MediaSnapshot, RecorderDefinition, RtmpCapabilities, RtmpRegistry, SessionId, StreamKey,
    TrackSnapshot,
};
use oxiroute_server::RtmpManagementApi;
use serde_json::Value;

#[test]
fn reports_truthful_empty_capabilities_before_rtmp_ingest_lands() {
    let api = RtmpManagementApi::new(Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    })));

    let response = api.handle("GET", "/api/v1/rtmp/streams", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(body["revision"], "0");
    assert_eq!(body["capabilities"]["live_ingest"], false);
    assert_eq!(body["capabilities"]["manual_recording"], false);
    assert_eq!(body["streams"], serde_json::json!([]));
}

#[test]
fn exposes_active_stream_media_and_recorder_state() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            publisher,
            vec![RecorderDefinition::manual(Some("archive".into()))],
            100,
        )
        .expect("publisher");
    registry
        .update_media_sample(
            stream_id,
            publisher,
            1,
            MediaSnapshot {
                audio: TrackSnapshot {
                    flv_codec_id: Some(10),
                    payload_bytes_received: 1_024,
                    last_rtmp_timestamp_ms: Some(120),
                    last_observed_at_unix_ms: Some(200),
                },
                video: TrackSnapshot {
                    flv_codec_id: Some(7),
                    payload_bytes_received: 4_096,
                    last_rtmp_timestamp_ms: Some(123),
                    last_observed_at_unix_ms: Some(200),
                },
                fanout_payload_bytes_queued: 8_192,
            },
            200,
        )
        .expect("media sample");
    let api = RtmpManagementApi::new(Arc::clone(&registry));

    let response = api.handle("GET", "/api/v1/rtmp/streams", 300);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(body["streams"][0]["id"], stream_id.to_string());
    assert_eq!(body["streams"][0]["application"], "live");
    assert_eq!(body["streams"][0]["name"], "camera");
    assert_eq!(body["streams"][0]["media"]["audio"]["codec_id"], 10);
    assert_eq!(
        body["streams"][0]["media"]["video"]["payload_bytes"],
        "4096"
    );
    assert_eq!(body["streams"][0]["recorders"][0]["phase"]["state"], "idle");
}

#[test]
fn recording_routes_are_capability_gated_and_use_exact_ids() {
    let unavailable_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    }));
    let publisher = SessionId::new();
    let stream_id = unavailable_registry
        .attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            publisher,
            vec![RecorderDefinition::manual(None)],
            100,
        )
        .expect("model publisher");
    let recorder_id = unavailable_registry.snapshot().streams[0].recorders[0].id;
    let unavailable = RtmpManagementApi::new(unavailable_registry);

    let path = format!("/api/v1/rtmp/streams/{stream_id}/recorders/{recorder_id}/start");
    assert_eq!(unavailable.handle("POST", &path, 200).status, 501);
    assert_eq!(
        unavailable
            .handle(
                "POST",
                "/api/v1/rtmp/streams/not-a-uuid/recorders/not-a-uuid/start",
                200,
            )
            .status,
        400
    );
}
