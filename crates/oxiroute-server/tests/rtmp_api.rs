use std::fs;
use std::sync::Arc;

use oxiroute_rtmp::{
    MediaSnapshot, RecorderDefinition, RtmpCapabilities, RtmpRegistry, SessionId, StreamKey,
    TrackSnapshot,
};
use oxiroute_server::{RtmpManagementApi, RuntimeMetrics};
use serde_json::Value;

#[test]
fn reports_truthful_empty_capabilities_when_ingest_is_disabled() {
    let api = RtmpManagementApi::new(
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        })),
        RuntimeMetrics::new(),
    );

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
    let api = RtmpManagementApi::new(Arc::clone(&registry), RuntimeMetrics::new());

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
    let unavailable = RtmpManagementApi::new(unavailable_registry, RuntimeMetrics::new());

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

#[test]
fn serves_prebuilt_ui_assets_without_request_time_filesystem_paths() {
    let directory = tempfile::tempdir().expect("temporary UI directory");
    fs::create_dir(directory.path().join("assets")).expect("asset directory");
    fs::write(
        directory.path().join("index.html"),
        "<main>Broadcast desk</main>",
    )
    .expect("index asset");
    fs::write(directory.path().join("assets/app.css"), "body{color:white}").expect("CSS asset");
    let api =
        RtmpManagementApi::with_ui_dir(empty_registry(), RuntimeMetrics::new(), directory.path())
            .expect("UI asset load");

    let index = api.handle("GET", "/", 100);
    assert_eq!(index.status, 200);
    assert_eq!(index.content_type, "text/html; charset=utf-8");
    assert_eq!(index.body, b"<main>Broadcast desk</main>");

    let css = api.handle("GET", "/assets/app.css", 100);
    assert_eq!(css.status, 200);
    assert_eq!(css.content_type, "text/css; charset=utf-8");
    assert_eq!(api.handle("GET", "/assets/../index.html", 100).status, 404);
}

#[test]
fn exposes_runtime_listener_and_rtmp_monitoring() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("live", "rtmp", "127.0.0.1:1935", 100)
        .expect("listener metrics");
    let connection = listener.begin_connection().expect("active connection");
    connection
        .record_bytes_received(4_096)
        .expect("received traffic");
    connection.record_bytes_sent(3_073).expect("sent traffic");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("live", "broadcast", "camera"),
            publisher,
            Vec::new(),
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
                    payload_bytes_received: 1_024,
                    ..TrackSnapshot::default()
                },
                video: TrackSnapshot {
                    payload_bytes_received: 8_192,
                    ..TrackSnapshot::default()
                },
                fanout_payload_bytes_queued: 0,
            },
            200,
        )
        .expect("media sample");
    let api = RtmpManagementApi::new(registry, metrics);

    let response = api.handle("GET", "/api/v1/monitoring", 300);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert!(body["sampledAtUnixMs"].as_u64().is_some());
    assert_eq!(body["traffic"]["acceptedConnections"], 1);
    assert_eq!(body["traffic"]["activeConnections"], 1);
    assert_eq!(body["traffic"]["bytesReceived"], 4_096);
    assert_eq!(body["traffic"]["bytesSent"], 3_073);
    assert_eq!(body["listeners"][0]["protocol"], "rtmp");
    assert_eq!(body["upstreamPools"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["rtmp"]["activeStreams"], 1);
    assert_eq!(body["rtmp"]["publishers"], 1);
    assert_eq!(body["rtmp"]["subscribers"], 0);
    assert_eq!(body["rtmp"]["mediaPayloadBytesReceived"], 9_216);
    assert_eq!(api.handle("POST", "/api/v1/monitoring", 300).status, 405);
}

fn empty_registry() -> Arc<RtmpRegistry> {
    Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    }))
}
