#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/rtmp.rs"]
mod rtmp_support;

use std::{
    fmt::Write as _,
    fs,
    net::{Ipv4Addr, SocketAddr},
    os::{
        fd::IntoRawFd,
        unix::fs::{PermissionsExt as _, symlink},
    },
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::{
    Certificate, CertificateSource, Config, Listener, Management, Protocol, RtmpAccessPolicy,
    RtmpApplication, RtmpDashPolicy, RtmpDashSegmentNaming, RtmpRecorderStart, RtmpService,
    RtmpSessionCeilings, RtmpTokenPolicy, RtmpTokenSource, RtmpVodPolicy, RtmpVodSource,
    render_lua,
};
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MediaSnapshot, RtmpApplication as RuntimeRtmpApplication,
    RtmpCapabilities, RtmpPushApplication, RtmpPushTarget, RtmpRegistry, RtmpRelayConfig,
    RtmpServiceRuntime, RtmpSessionControlAction, RtmpSessionPolicy, SessionId, StreamKey,
    TrackSnapshot, VideoCodecIdentifier,
};
use oxiroute_server::{
    HttpListenerApp, RtmpManagementApi, RuntimeMetrics, ServiceKind, TopologySnapshot,
    config_coordinator::{
        CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigRevision, MAX_CANONICAL_CONFIG_BYTES,
    },
    runtime_plan,
};
use pingora::{
    server::Fds,
    services::{Service as PingoraService, listening::Service as ListeningService},
};
use serde_json::Value;
use tempfile::TempDir;

use rtmp_support::RtmpSessionClient;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    sync::{Mutex as TokioMutex, watch},
    task::JoinHandle,
};

use config_support::{empty_config, loopback_address, rtmp_recorder, socket_bind};
use fixture_support::{fixture, write_file_with_mode};
use http_support::{HttpResponse, http_request, raw_http_request};

const TEST_TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

#[test]
fn reports_truthful_empty_capabilities_when_ingest_is_disabled() {
    let api = management_api(
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
fn reports_bounded_rtmp_global_and_live_stats_without_stream_queries() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("edge", "live", "camera"),
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
                    payload_bytes_received: 128,
                    ..TrackSnapshot::default()
                },
                ..MediaSnapshot::default()
            },
            200,
        )
        .expect("media sample");
    let api = management_api(registry, RuntimeMetrics::new());

    let global = api.handle("GET", "/api/v1/rtmp/stats/global", 300);
    let global: Value = serde_json::from_slice(&global.body).expect("global stats JSON");
    assert_eq!(global["global"]["activeStreams"], 1);
    assert_eq!(global["global"]["publishers"], 1);
    assert_eq!(global["global"]["audioPayloadBytes"], "128");

    let live = api.handle("GET", "/api/v1/rtmp/stats/live", 300);
    let live: Value = serde_json::from_slice(&live.body).expect("live stats JSON");
    assert_eq!(live["live"].as_array().map(Vec::len), Some(1));
    assert_eq!(live["live"][0]["application"], "live");
    assert_eq!(live["live"][0]["name"], "camera");
    assert!(!live.to_string().contains("?"));
}

#[tokio::test]
async fn queues_target_checked_publisher_disconnects_through_the_management_api() {
    let active = editable_config();
    let harness = ManagementHarness::start(&candidate_config(&active, "live")).await;
    let mut client = RtmpSessionClient::connect(harness.rtmp_runtime(), "broadcast");
    client.publish("camera", 100);
    let snapshot = client.server.client_snapshot().expect("client snapshot");
    assert_eq!(snapshot.role, oxiroute_rtmp::RtmpSessionRole::Publisher);

    let stats = harness
        .request("GET", "/api/v1/rtmp/stats/clients", None, None)
        .await;
    assert_eq!(stats.status, 200);
    assert_eq!(
        stats.json()["clients"][0]["id"],
        snapshot.session_id.to_string()
    );
    assert_eq!(
        stats.json()["clients"][0]["revision"],
        snapshot.revision.to_string()
    );

    let control_path = format!(
        "/api/v1/rtmp/clients/{}/publisher/drop",
        snapshot.session_id
    );
    let authorization = format!("Bearer {TEST_TOKEN}");
    let missing_revision = http_request(
        harness.address,
        "POST",
        &control_path,
        &[("Authorization", authorization.as_str())],
        &[],
    )
    .await;
    assert_eq!(missing_revision.status, 428);

    let revision = snapshot.revision.to_string();
    let stale_revision = snapshot.revision.saturating_add(1).to_string();
    let stale = http_request(
        harness.address,
        "POST",
        &control_path,
        &[
            ("Authorization", authorization.as_str()),
            ("If-Rtmp-Session-Revision", stale_revision.as_str()),
        ],
        &[],
    )
    .await;
    assert_eq!(stale.status, 409);

    let response = http_request(
        harness.address,
        "POST",
        &control_path,
        &[
            ("Authorization", authorization.as_str()),
            ("If-Rtmp-Session-Revision", revision.as_str()),
        ],
        &[],
    )
    .await;
    assert_eq!(response.status, 202);
    assert_eq!(response.json()["target"], "publisher");
    assert_eq!(
        client.server.take_control_action(),
        Some(RtmpSessionControlAction::Publisher)
    );
}

#[test]
fn reports_http3_only_when_a_listener_is_active() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("reverse-h3", "http3", "127.0.0.1:9443", Some(64))
        .expect("HTTP/3 listener metrics");
    listener.mark_listening();
    let api = management_api(empty_registry(), metrics);

    let response = api.handle("GET", "/api/v1/capabilities", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(body["http3"]["reverse"]["status"], "active");
    assert_eq!(body["http3"]["reverse"]["listeners"], serde_json::json!(["reverse-h3"]));
    assert_eq!(body["http3"]["reverse"]["fallback"], "none");
    assert_eq!(body["http3"]["forward"]["status"], "unconfigured");
}

#[tokio::test]
async fn serves_authenticated_vod_objects_and_single_ranges() {
    let directory = TempDir::new().expect("VOD directory");
    fs::write(directory.path().join("movie.flv"), b"0123456789").expect("VOD object");
    let mut config = candidate_config(&empty_config(), "live");
    config.rtmp_services[0].applications[0].vod = Some(RtmpVodPolicy {
        sources: vec![RtmpVodSource::Local {
            name: "archive".into(),
            root_directory: directory.path().to_path_buf(),
        }],
        max_sessions: 2,
        max_file_bytes: 1_024,
        max_duration_ms: 60_000,
    });
    let harness = ManagementHarness::start(&config).await;
    let path = "/api/v1/rtmp/vod/live/broadcast/archive/movie.flv";

    let unauthorized = harness
        .request_with("GET", path, None, None, None, None)
        .await;
    assert_eq!(unauthorized.status, 401);

    let full = harness.request("GET", path, None, None).await;
    assert_eq!(full.status, 200);
    assert_eq!(full.body(), b"0123456789");
    assert_eq!(full.header("accept-ranges"), Some("bytes"));
    assert_eq!(full.header("content-range"), None);
    assert_eq!(full.header("content-type"), Some("video/x-flv"));

    let authorization = format!("Bearer {TEST_TOKEN}");
    let ranged = http_request(
        harness.address,
        "GET",
        path,
        &[("Authorization", authorization.as_str()), ("Range", "bytes=2-5")],
        &[],
    )
    .await;
    assert_eq!(ranged.status, 206);
    assert_eq!(ranged.body(), b"2345");
    assert_eq!(ranged.header("content-range"), Some("bytes 2-5/10"));

    let multiple = http_request(
        harness.address,
        "GET",
        path,
        &[
            ("Authorization", authorization.as_str()),
            ("Range", "bytes=0-1,4-5"),
        ],
        &[],
    )
    .await;
    assert_eq!(multiple.status, 416);
    assert_eq!(multiple.header("content-range"), Some("bytes */10"));
}

#[tokio::test]
async fn serves_authenticated_dash_manifests_segments_and_single_ranges() {
    let media_root = TempDir::new().expect("DASH media root");
    let mut config = candidate_config(&empty_config(), "live");
    config.rtmp_services[0].applications[0].dash = Some(RtmpDashPolicy {
        root_directory: media_root.path().to_path_buf(),
        segment_duration_ms: 1_000,
        max_segment_duration_ms: 2_000,
        playlist_length_ms: 6_000,
        segment_naming: RtmpDashSegmentNaming::Sequential,
        nested: true,
        cleanup: true,
        max_segment_bytes: 1024 * 1024,
        max_queue_messages: 16,
        max_storage_bytes: 4 * 1024 * 1024,
        max_storage_files: 64,
        max_active_streams: 2,
    });
    let harness = ManagementHarness::start(&config).await;
    let mut publisher = RtmpSessionClient::connect(harness.rtmp_runtime(), "broadcast");
    publisher.publish("camera", 1_000);
    publisher.publish_audio(0, &[0xaf, 0, 0x12, 0x10], 1_001);
    publisher.publish_video(
        0,
        &[0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1,
            0, 2, 0x68, 0xce],
        1_002,
    );
    publisher.publish_video(0, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 1], 1_003);
    publisher.publish_audio(0, &[0xaf, 1, 2, 3, 4], 1_004);
    publisher.publish_video(1_000, &[0x27, 1, 0, 0, 0, 0, 0, 0, 2, 0x41, 2], 1_005);
    publisher.publish_video(2_000, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 3], 1_006);

    let manifest_path = "/api/v1/rtmp/media/live/broadcast/camera/dash/manifest.mpd";
    let deadline = Instant::now() + Duration::from_secs(2);
    let manifest = loop {
        let response = harness.request("GET", manifest_path, None, None).await;
        if response.status == 200 {
            break response;
        }
        assert!(Instant::now() < deadline, "DASH manifest publication timeout");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(manifest.header("content-type"), Some("application/dash+xml"));
    let manifest_body = String::from_utf8(manifest.body.clone()).expect("MPD UTF-8");
    assert!(manifest_body.contains("seg-0.m4s"));
    assert!(manifest_body.contains("type=\"dynamic\""));

    let unauthorized = harness
        .request_with("GET", manifest_path, None, None, None, None)
        .await;
    assert_eq!(unauthorized.status, 401);

    let segment_path = "/api/v1/rtmp/media/live/broadcast/camera/dash/seg-0.m4s";
    let authorization = format!("Bearer {TEST_TOKEN}");
    let ranged = http_request(
        harness.address,
        "GET",
        segment_path,
        &[("Authorization", authorization.as_str()), ("Range", "bytes=0-7")],
        &[],
    )
    .await;
    assert_eq!(ranged.status, 206);
    assert_eq!(ranged.body().len(), 8);
    assert!(ranged
        .header("content-range")
        .is_some_and(|value| value.starts_with("bytes 0-7/")));
}

#[test]
fn exposes_enhanced_video_codec_identity_without_claiming_recording_support() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("edge", "live", "hevc-camera"),
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
                video: TrackSnapshot {
                    video_codec: Some(VideoCodecIdentifier::FourCc(*b"hvc1")),
                    payload_bytes_received: 4_096,
                    ..TrackSnapshot::default()
                },
                ..MediaSnapshot::default()
            },
            200,
        )
        .expect("media sample");
    let api = management_api(registry, RuntimeMetrics::new());

    let response = api.handle("GET", "/api/v1/rtmp/streams", 300);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");
    let video = &body["streams"][0]["media"]["video"];
    assert!(video["codec_id"].is_null());
    assert_eq!(video["codec_fourcc"], "hvc1");
    assert_eq!(video["codec_name"], "hevc");
    assert_eq!(video["recording_supported"], false);
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
    let api = RtmpManagementApi::with_ui_dir(
        empty_registry(),
        RuntimeMetrics::new(),
        empty_topology(),
        directory.path(),
    )
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
    let api = management_api(registry, metrics);

    let response = api.handle("GET", "/api/v1/monitoring", 300);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert!(body["sampledAtUnixMs"].as_u64().is_some());
    assert_eq!(body["traffic"]["acceptedConnections"], "1");
    assert_eq!(body["traffic"]["activeConnections"], 1);
    assert_eq!(body["traffic"]["bytesReceived"], "4096");
    assert_eq!(body["traffic"]["bytesSent"], "3073");
    assert_eq!(body["listeners"][0]["protocol"], "rtmp");
    assert_eq!(body["upstreamPools"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["certbotCertificates"], serde_json::json!([]));
    assert!(body["certbotWatcher"].is_null());
    assert_eq!(body["rtmp"]["activeStreams"], 1);
    assert_eq!(body["rtmp"]["publishers"], 1);
    assert_eq!(body["rtmp"]["subscribers"], 0);
    assert_eq!(body["rtmp"]["mediaPayloadBytesReceived"], "9216");
    assert_eq!(body["rtmp"]["recorderBytesWritten"], "0");
    assert_eq!(body["rtmp"]["recorderSegmentsStarted"], "0");
    assert_eq!(body["rtmp"]["recorderSegmentsCompleted"], "0");
    assert_eq!(body["rtmp"]["recorderDiscontinuities"], "0");
    assert_eq!(api.handle("POST", "/api/v1/monitoring", 300).status, 405);
}

#[test]
fn relay_state_and_counters_are_observable_without_stream_queries() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve absent relay port");
    let destination = listener.local_addr().expect("relay destination");
    drop(listener);
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RuntimeRtmpApplication::with_runtime(
            "broadcast",
            true,
            true,
            LiveHub::new(LiveHubLimits::default()),
            [RtmpPushTarget {
                address: destination,
                host: destination.ip().to_string(),
                transport: oxiroute_rtmp::RtmpTransport::Rtmp,
                application: RtmpPushApplication::StreamName,
                stream_name: None,
                options: oxiroute_rtmp::RtmpClientOptions::default(),
                config: RtmpRelayConfig {
                    connect_timeout: Duration::from_millis(10),
                    reconnect_interval: Duration::from_millis(20),
                    ..RtmpRelayConfig::default()
                },
            }],
            [],
        )]),
    );
    let mut publisher = RtmpSessionClient::connect(&runtime, "broadcast");
    publisher.publish("camera?token=relay-wire-secret", 100);
    publisher.publish_audio(1, &[0xaf, 0x01, 0x44], 101);
    let deadline = Instant::now() + Duration::from_secs(1);
    while registry.snapshot().streams[0].relays[0]
        .status
        .connection_attempts
        < 2
    {
        assert!(Instant::now() < deadline, "relay retry timeout");
        thread::sleep(Duration::from_millis(5));
    }
    let api = management_api(Arc::clone(&registry), RuntimeMetrics::new());

    let catalog = api.handle("GET", "/api/v1/rtmp/streams", 200);
    let monitoring = api.handle("GET", "/api/v1/monitoring", 200);
    let catalog_json: Value = serde_json::from_slice(&catalog.body).expect("catalog JSON");
    let monitoring_json: Value = serde_json::from_slice(&monitoring.body).expect("monitoring JSON");
    assert_eq!(catalog_json["streams"][0]["relays"][0]["phase"], "backoff");
    assert_eq!(
        catalog_json["streams"][0]["relays"][0]["destination"]["application"],
        "camera"
    );
    assert!(
        monitoring_json["rtmp"]["relayConnectionAttempts"]
            .as_str()
            .is_some_and(|attempts| attempts.parse::<u64>().is_ok_and(|attempts| attempts >= 2))
    );
    assert_eq!(
        monitoring_json["rtmp"]["relays"][0]["lastFailure"],
        "connect"
    );
    let wire = format!(
        "{}{}",
        String::from_utf8_lossy(&catalog.body),
        String::from_utf8_lossy(&monitoring.body)
    );
    assert!(!wire.contains("relay-wire-secret"));
    publisher.server.close(300).expect("publisher close");
}

#[test]
fn monitoring_response_preserves_large_rtmp_cumulative_totals() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("live", "broadcast", "large-counter"),
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
                    payload_bytes_received: u64::MAX,
                    ..TrackSnapshot::default()
                },
                ..MediaSnapshot::default()
            },
            200,
        )
        .expect("media sample");
    let api = management_api(registry, RuntimeMetrics::new());

    let response = api.handle("GET", "/api/v1/monitoring", 300);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(
        body["rtmp"]["mediaPayloadBytesReceived"],
        u64::MAX.to_string()
    );
}

#[tokio::test]
async fn config_routes_require_the_injected_bearer_token() {
    let harness = ManagementHarness::start(&editable_config()).await;

    for (method, path) in [
        ("GET", "/api/v1/config"),
        ("POST", "/api/v1/config/validate"),
        ("PUT", "/api/v1/config"),
        ("GET", "/api/v1/rtmp/stats"),
        ("GET", "/api/v1/audit"),
        ("GET", "/api/v1/audit/status"),
    ] {
        let response = harness
            .request_with(method, path, None, None, None, None)
            .await;
        assert_eq!(response.status, 401);
        assert_eq!(response.json()["error"]["code"], "unauthorized");
        assert_eq!(
            response.headers.get("www-authenticate").map(String::as_str),
            Some("Bearer")
        );
        assert!(!response.headers.contains_key("access-control-allow-origin"));
    }

    let wrong = harness
        .request_with(
            "GET",
            "/api/v1/config",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(wrong.status, 401);

    let authorized = harness.request("GET", "/api/v1/config", None, None).await;
    assert_eq!(authorized.status, 200);
    assert!(authorized.headers.contains_key("x-correlation-id"));
    assert_eq!(
        harness
            .request_with("GET", "/api/v1/topology", None, None, None, None)
            .await
            .status,
        401
    );
    assert_eq!(
        harness
            .request("GET", "/api/v1/topology", None, None)
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn event_stream_requires_the_management_bearer_token() {
    let harness = ManagementHarness::start(&editable_config()).await;

    let missing = harness
        .request_with("GET", "/api/v1/events/stream", None, None, None, None)
        .await;
    assert_eq!(missing.status, 401);
    assert_eq!(missing.json()["error"]["code"], "unauthorized");
    assert_eq!(missing.header("www-authenticate"), Some("Bearer"));

    let wrong = harness
        .request_with(
            "GET",
            "/api/v1/events/stream",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(wrong.status, 401);
}

#[tokio::test]
async fn event_stream_sends_an_initial_cursor_without_replaying_old_events() {
    let harness = ManagementHarness::start(&editable_config()).await;
    let (mut stream, head) = harness.open_event_stream(None).await;

    let head = String::from_utf8(head).expect("SSE response headers");
    assert!(head.starts_with("HTTP/1.1 200"));
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/event-stream")
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
    );
    assert!(head.to_ascii_lowercase().contains("x-correlation-id: op-"));

    let frame = read_chunk(&mut stream).await;
    let frame = String::from_utf8(frame).expect("initial SSE frame");
    assert!(frame.starts_with("event: ready\ndata: {\"cursor\":"));
    assert!(frame.ends_with("}\n\n"));

    drop(stream);
}

#[tokio::test]
async fn event_stream_reconnects_from_last_event_id() {
    let harness = ManagementHarness::start(&editable_config()).await;
    let (mut stream, _) = harness.open_event_stream(Some(42)).await;

    let frame = String::from_utf8(read_chunk(&mut stream).await).expect("reconnect SSE frame");
    assert_eq!(frame, "event: ready\ndata: {\"cursor\":42}\n\n");

    drop(stream);
}

#[tokio::test]
async fn event_stream_client_cancellation_does_not_block_following_requests() {
    let harness = ManagementHarness::start(&editable_config()).await;
    let (mut stream, _) = harness.open_event_stream(None).await;
    let _ = read_chunk(&mut stream).await;
    drop(stream);
    tokio::task::yield_now().await;

    assert_eq!(
        harness
            .request("GET", "/api/v1/config", None, None)
            .await
            .status,
        200
    );
}

#[test]
fn management_token_files_are_bounded_regular_nofollow_and_owner_only() {
    let directory = TempDir::new().expect("token directory");
    let config_path = directory.path().join("oxiroute.lua");
    let config = editable_config();
    fs::write(&config_path, render_lua(&config).unwrap()).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&config_path).unwrap();
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("config must load")
    };
    let token_path = write_file_with_mode(
        directory.path(),
        "management.token",
        format!("{TEST_TOKEN}\n").as_bytes(),
        0o600,
    );

    assert!(
        RtmpManagementApi::new(empty_registry(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator.clone(),
                document.candidate_revision.clone(),
                &token_path,
            )
            .is_ok()
    );

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        RtmpManagementApi::new(empty_registry(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator.clone(),
                document.candidate_revision.clone(),
                &token_path,
            )
            .is_err()
    );

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).unwrap();
    let token_link = directory.path().join("management-token-link");
    symlink(&token_path, &token_link).unwrap();
    assert!(
        RtmpManagementApi::new(empty_registry(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator.clone(),
                document.candidate_revision.clone(),
                &token_link,
            )
            .is_err()
    );

    fs::write(&token_path, format!("{TEST_TOKEN}\n\n")).unwrap();
    assert!(
        RtmpManagementApi::new(empty_registry(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator,
                document.candidate_revision,
                &token_path,
            )
            .is_err()
    );
}

#[tokio::test]
async fn config_writes_require_a_current_revision_and_return_authoritative_conflicts() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let candidate = candidate_config(&active, "submitted-ingest");
    let request = serde_json::json!({ "config": candidate });

    let missing = harness
        .request("PUT", "/api/v1/config", None, Some(&request))
        .await;
    assert_eq!(missing.status, 428);
    assert_eq!(missing.json()["error"]["code"], "precondition_required");
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active).unwrap().as_bytes()
    );

    let authoritative = candidate_config(&active, "external-ingest");
    fs::write(
        &harness.config_path,
        render_lua(&authoritative).expect("authoritative config renders"),
    )
    .expect("replace canonical config externally");
    let conflict = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;

    assert_eq!(conflict.status, 409);
    let conflict_json = conflict.json();
    assert_eq!(conflict_json["outcome"], "conflict");
    assert_eq!(
        conflict_json["activeRevision"],
        harness.active_revision.as_str()
    );
    assert_eq!(
        conflict_json["config"]["listeners"][0]["name"],
        "external-ingest"
    );
    assert_ne!(
        conflict_json["diskRevision"],
        harness.active_revision.as_str()
    );
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&authoritative).unwrap().as_bytes()
    );
}

#[tokio::test]
async fn config_api_reports_source_format_composition_and_native_preview_name() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;

    let get = harness.request("GET", "/api/v1/config", None, None).await;
    assert_eq!(get.status, 200);
    let get = get.json();
    assert_eq!(get["configFormat"], "lua");
    assert_eq!(get["compositional"], false);
    assert_eq!(get["dependencyCount"], 0);
    assert_eq!(get["configPreview"], get["luaPreview"]);
    assert_eq!(get["candidateRevision"], get["activeRevision"]);
    assert_ne!(get["diskRevision"], get["candidateRevision"]);

    let request = serde_json::json!({ "config": active });
    let validated = harness
        .request("POST", "/api/v1/config/validate", None, Some(&request))
        .await;
    assert_eq!(validated.status, 200);
    let validated = validated.json();
    assert_eq!(validated["configFormat"], "lua");
    assert_eq!(validated["configPreview"], validated["luaPreview"]);
}

#[tokio::test]
async fn config_api_redacts_rtmp_token_secrets_from_typed_and_rendered_views() {
    let mut active = editable_config();
    active.rtmp_services = vec![RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
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
                    secret: "super-secret-token".into(),
                }),
            },
            play: RtmpAccessPolicy::default(),
            limits: RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: Vec::new(),
        }],
    }];
    let harness = ManagementHarness::start(&active).await;

    let get = harness.request("GET", "/api/v1/config", None, None).await;
    assert_eq!(get.status, 200);
    let get_json = get.json();
    assert_eq!(
        get_json["config"]["rtmp_services"][0]["applications"][0]["publish"]["token"]["secret"],
        "<redacted>"
    );
    assert!(get_json["configPreview"].as_str().is_some_and(|preview| {
        preview.contains("<redacted>") && !preview.contains("super-secret-token")
    }));
    assert!(!String::from_utf8_lossy(&get.body).contains("super-secret-token"));

    let request = serde_json::json!({ "config": active });
    let validated = harness
        .request("POST", "/api/v1/config/validate", None, Some(&request))
        .await;
    assert_eq!(validated.status, 200);
    let validated_json = validated.json();
    assert_eq!(
        validated_json["normalizedConfig"]["rtmp_services"][0]["applications"][0]["publish"]["token"]
            ["secret"],
        "<redacted>"
    );
    assert!(!String::from_utf8_lossy(&validated.body).contains("super-secret-token"));
}

#[tokio::test]
async fn config_api_rejects_typed_save_over_a_compositional_root() {
    let config = editable_config();
    let rendered = serde_json::json!({
        "templates": { "base": serde_json::to_value(&config).unwrap() },
        "use": "base",
    });
    let source = serde_json::to_vec_pretty(&rendered).unwrap();
    let harness = ManagementHarness::start_source(&config, "hocon", &source).await;
    let before = fs::read(&harness.config_path).unwrap();
    let get = harness.request("GET", "/api/v1/config", None, None).await;
    let snapshot = get.json();
    assert_eq!(snapshot["configFormat"], "hocon");
    assert_eq!(snapshot["compositional"], true);

    let request = serde_json::json!({ "config": config });
    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            snapshot["diskRevision"].as_str(),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 422);
    assert_eq!(
        response.json()["diagnostics"][0]["code"],
        "E_COMPOSITIONAL_ROOT"
    );
    assert_eq!(fs::read(&harness.config_path).unwrap(), before);
}

#[tokio::test]
async fn rejects_invalid_malformed_and_oversized_config_requests() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let mut invalid = active.clone();
    invalid.version = 2;
    let invalid_request = serde_json::json!({ "config": invalid });

    let validation = harness
        .request(
            "POST",
            "/api/v1/config/validate",
            None,
            Some(&invalid_request),
        )
        .await;
    assert_eq!(validation.status, 422);
    assert_eq!(validation.json()["diagnostics"][0]["severity"], "error");

    let save = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&invalid_request),
        )
        .await;
    assert_eq!(save.status, 422);
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active).unwrap().as_bytes()
    );

    let malformed = harness
        .raw_request(
            format!(
                "POST /api/v1/config/validate HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{{"
            )
            .into_bytes(),
        )
        .await;
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.json()["error"]["code"], "malformed_json");

    let oversized = harness
        .raw_request(
            format!(
                "POST /api/v1/config/validate HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_CANONICAL_CONFIG_BYTES + 1
            )
            .into_bytes(),
        )
        .await;
    assert_eq!(oversized.status, 413);
    assert_eq!(oversized.json()["error"]["code"], "config_body_too_large");
}

#[tokio::test]
async fn config_routes_reject_wrong_methods_and_paths() {
    let harness = ManagementHarness::start(&editable_config()).await;

    let config_method = harness.request("POST", "/api/v1/config", None, None).await;
    assert_eq!(config_method.status, 405);
    assert_eq!(
        config_method.headers.get("allow").map(String::as_str),
        Some("GET, PUT")
    );

    let validate_method = harness
        .request("GET", "/api/v1/config/validate", None, None)
        .await;
    assert_eq!(validate_method.status, 405);
    assert_eq!(
        validate_method.headers.get("allow").map(String::as_str),
        Some("POST")
    );

    let missing = harness
        .request("GET", "/api/v1/config/extra", None, None)
        .await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.json()["error"]["code"], "route_not_found");
}

#[tokio::test]
async fn config_routes_require_exact_paths_json_media_and_raw_revision_headers() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let request = serde_json::json!({ "config": candidate_config(&active, "candidate") });

    for path in [
        "/api/v1/config/",
        "/api//v1/config",
        "/api/v1/config/validate/",
        "/api/v1//config/validate",
    ] {
        assert_eq!(harness.request("GET", path, None, None).await.status, 404);
    }

    for content_type in [None, Some("text/plain")] {
        let response = harness
            .request_with(
                "POST",
                "/api/v1/config/validate",
                Some(TEST_TOKEN),
                None,
                content_type,
                Some(&request),
            )
            .await;
        assert_eq!(response.status, 415);
        assert_eq!(response.json()["error"]["code"], "unsupported_media_type");
    }
    assert_eq!(
        harness
            .request_with(
                "POST",
                "/api/v1/config/validate",
                Some(TEST_TOKEN),
                None,
                Some("application/json; charset=utf-8"),
                Some(&request),
            )
            .await
            .status,
        200
    );

    let missing_revision = harness
        .request("PUT", "/api/v1/config", None, Some(&request))
        .await;
    assert_eq!(missing_revision.status, 428);
    let malformed_revision = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(&format!("\"{}\"", harness.active_revision.as_str())),
            Some(&request),
        )
        .await;
    assert_eq!(malformed_revision.status, 400);
    assert_eq!(
        malformed_revision.json()["error"]["code"],
        "invalid_config_revision"
    );

    let legacy_header = harness
        .raw_request(
            format!(
                "PUT /api/v1/config HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nIf-Match: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                harness.active_revision.as_str(),
                request.to_string().len(),
                request,
            )
            .into_bytes(),
        )
        .await;
    assert_eq!(legacy_header.status, 428);
}

#[tokio::test]
async fn direct_put_preflight_failure_does_not_mutate_disk() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let mut candidate = active.clone();
    let CertificateSource::Files {
        private_key_path, ..
    } = &mut candidate.certificates[0].source
    else {
        panic!("direct-file certificate")
    };
    *private_key_path = fixture("proxy-b-key.pem");
    let request = serde_json::json!({ "config": candidate });

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 422);
    assert_eq!(
        response.json()["diagnostics"][0]["code"],
        "E_RUNTIME_PREPARE"
    );
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active).unwrap().as_bytes()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn candidate_recorder_preflight_never_mutates_the_recording_root() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let root = TempDir::new().expect("recording root");
    let mut candidate = candidate_config(&active, "candidate-recorder");
    let mut recorder = rtmp_recorder("archive", RtmpRecorderStart::Continuous, root.path());
    recorder.suffix_template = "-%Y-private.flv".into();
    candidate.rtmp_services[0].applications[0]
        .recorders
        .push(recorder);
    let request = serde_json::json!({ "config": candidate });

    let validation = harness
        .request("POST", "/api/v1/config/validate", None, Some(&request))
        .await;
    assert_eq!(validation.status, 200);
    assert!(
        fs::read_dir(root.path())
            .expect("root entries")
            .next()
            .is_none()
    );
    let validation_json = validation.json();
    let topology = validation_json["topology"].to_string();
    assert!(!topology.contains(&root.path().display().to_string()));
    assert!(!topology.contains("private.flv"));
    let listener = validation_json["topology"]["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["kind"] == "rtmp_listener"))
        .expect("candidate RTMP listener");
    assert_eq!(
        listener["attributes"]["applications"][0]["recording"]["recorderCount"],
        1
    );
    assert_eq!(
        listener["attributes"]["applications"][0]["recording"]["continuousRecorderCount"],
        1
    );

    let saved = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(saved.status, 200);
    assert!(
        fs::read_dir(root.path())
            .expect("root entries")
            .next()
            .is_none()
    );
    assert!(!root.path().join(".oxiroute-recording.lock").exists());
}

#[tokio::test]
async fn missing_candidate_ui_assets_are_rejected_without_mutating_disk() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let mut candidate = active.clone();
    candidate.management = Some(Management {
        bind: "127.0.0.1:9090".parse().unwrap(),
        ui_dir: Some(harness.config_path.with_file_name("missing-ui")),
    });
    let request = serde_json::json!({ "config": candidate });

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 422);
    assert_eq!(response.json()["diagnostics"][0]["code"], "E_UI_ASSETS");
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active).unwrap().as_bytes()
    );
}

#[tokio::test]
async fn saving_the_active_generation_is_truthfully_idempotent() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let request = serde_json::json!({ "config": active });

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 200);
    let response_json = response.json();
    assert_eq!(response_json["outcome"], "unchanged_active");
    assert_eq!(response_json["activationState"], "active");
    assert_eq!(response_json["restartRequired"], false);
    assert_eq!(
        response_json["candidateRevision"],
        response_json["activeRevision"]
    );
    assert_ne!(
        response_json["diskRevision"],
        response_json["activeRevision"]
    );
}

#[tokio::test]
async fn conflict_reload_failure_returns_no_fabricated_revision() {
    let active = editable_config();
    let harness = ManagementHarness::start(&active).await;
    let request = serde_json::json!({ "config": candidate_config(&active, "candidate") });
    fs::write(&harness.config_path, "not valid canonical Lua").unwrap();

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 503);
    let response_json = response.json();
    assert!(response_json["diskRevision"].is_null());
    assert_eq!(response_json["outcome"], "authoritative_state_unavailable");
    assert_eq!(
        response_json["error"]["code"],
        "authoritative_config_unavailable"
    );
}

#[tokio::test]
async fn chunked_config_body_is_bounded_while_streaming() {
    let harness = ManagementHarness::start(&editable_config()).await;
    let mut request = format!(
        "POST /api/v1/config/validate HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )
    .into_bytes();
    let chunk = vec![b'a'; 64 * 1024];
    for _ in 0..16 {
        request.extend_from_slice(b"10000\r\n");
        request.extend_from_slice(&chunk);
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"1\r\na\r\n0\r\n\r\n");

    let response = harness.raw_request(request).await;
    assert_eq!(response.status, 413);
    assert_eq!(response.json()["error"]["code"], "config_body_too_large");
}

fn empty_registry() -> Arc<RtmpRegistry> {
    Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    }))
}

fn management_api(registry: Arc<RtmpRegistry>, metrics: RuntimeMetrics) -> RtmpManagementApi {
    RtmpManagementApi::new(registry, metrics, empty_topology())
}

fn empty_topology() -> Arc<TopologySnapshot> {
    runtime_plan(&empty_config())
        .expect("empty runtime plan")
        .topology
}

fn editable_config() -> Config {
    Config {
        certificates: vec![Certificate {
            name: "test-certificate".into(),
            dns_names: vec!["proxy.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path: fixture("proxy-a.pem"),
                private_key_path: fixture("proxy-a-key.pem"),
            },
        }],
        ..empty_config()
    }
}

fn candidate_config(active: &Config, listener_name: &str) -> Config {
    let mut candidate = active.clone();
    candidate.listeners.push(Listener {
        name: listener_name.into(),
        bind: socket_bind(loopback_address(1935)),
        protocol: Protocol::Rtmp,
        service: Some("live".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(100),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    candidate.rtmp_services.push(RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
        exec_profiles: Vec::new(),
        applications: vec![RtmpApplication {
            name: "broadcast".into(),
            live: true,
            idle_streams: false,
            publish: oxiroute_config::RtmpAccessPolicy::default(),
            play: oxiroute_config::RtmpAccessPolicy::default(),
            limits: oxiroute_config::RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: Vec::new(),
        }],
    });
    candidate
}

struct ManagementHarness {
    active_revision: ConfigRevision,
    disk_revision: ConfigRevision,
    address: SocketAddr,
    config_path: PathBuf,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    rtmp_runtime: Option<RtmpServiceRuntime>,
    _directory: TempDir,
}

impl ManagementHarness {
    async fn start(config: &Config) -> Self {
        let source = render_lua(config).expect("active config renders");
        Self::start_source(config, "lua", source.as_bytes()).await
    }

    async fn start_source(config: &Config, extension: &str, source: &[u8]) -> Self {
        let directory = TempDir::new().expect("temporary canonical config directory");
        let config_path = directory.path().join(format!("oxiroute.{extension}"));
        fs::write(&config_path, source).expect("write active config");
        let coordinator = CanonicalConfigCoordinator::new(&config_path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("active config must load")
        };
        let active_revision = document.candidate_revision.clone();
        let disk_revision = document.disk_revision.clone();
        let plan = runtime_plan(config).expect("active runtime plan");
        let registry = Arc::new(RtmpRegistry::new(plan.rtmp_capabilities));
        let rtmp_runtime = plan.services.iter().find_map(|service| match &service.kind {
            ServiceKind::Rtmp(service) => Some(
                service
                    .runtime(Arc::clone(&registry))
                    .expect("RTMP test runtime"),
            ),
            _ => None,
        });
        let metrics = RuntimeMetrics::new();
        metrics.set_rtmp_recording_supported(plan.rtmp_recording_supported);
        let api = RtmpManagementApi::new(Arc::clone(&registry), metrics, Arc::clone(&plan.topology))
            .with_config_coordinator(coordinator, active_revision.clone(), TEST_TOKEN)
            .expect("injected management token")
            .with_vod_catalog(Arc::clone(&plan.rtmp_vod_catalog))
            .with_media_catalog(Arc::clone(&plan.rtmp_media_catalog));

        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("reserve management listener");
        listener
            .set_nonblocking(true)
            .expect("make management listener nonblocking");
        let address = listener.local_addr().expect("management listener address");
        let http_server = HttpListenerApp::new(api.into_http_app(), None);
        let mut service = ListeningService::new("OxiRoute management API test".into(), http_server);
        service.add_tcp(&address.to_string());
        let mut inherited = Fds::new();
        inherited.add(address.to_string(), listener.into_raw_fd());
        let inherited = Arc::new(TokioMutex::new(inherited));
        let (shutdown, shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            PingoraService::start_service(&mut service, Some(inherited), shutdown_watch, 1).await;
        });
        tokio::task::yield_now().await;

        Self {
            active_revision,
            disk_revision,
            address,
            config_path,
            shutdown,
            task,
            rtmp_runtime,
            _directory: directory,
        }
    }

    fn rtmp_runtime(&self) -> &RtmpServiceRuntime {
        self.rtmp_runtime
            .as_ref()
            .expect("RTMP service runtime in test configuration")
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        config_revision: Option<&str>,
        body: Option<&Value>,
    ) -> HttpResponse {
        self.request_with(
            method,
            path,
            Some(TEST_TOKEN),
            config_revision,
            body.map(|_| "application/json"),
            body,
        )
        .await
    }

    async fn request_with(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        config_revision: Option<&str>,
        content_type: Option<&str>,
        body: Option<&Value>,
    ) -> HttpResponse {
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut headers = Vec::new();
        if let Some(token) = token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        if let Some(config_revision) = config_revision {
            headers.push(("If-Config-Revision", config_revision.to_owned()));
        }
        if let Some(content_type) = content_type {
            headers.push(("Content-Type", content_type.to_owned()));
        }
        let headers = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect::<Vec<_>>();
        http_request(self.address, method, path, &headers, body.as_bytes()).await
    }

    async fn raw_request(&self, request: Vec<u8>) -> HttpResponse {
        raw_http_request(self.address, &request).await
    }

    async fn open_event_stream(&self, last_event_id: Option<u64>) -> (TcpStream, Vec<u8>) {
        let mut stream = TcpStream::connect(self.address)
            .await
            .expect("connect event stream");
        let mut request = format!(
            "GET /api/v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nAccept: text/event-stream\r\nConnection: close\r\n"
        );
        if let Some(last_event_id) = last_event_id {
            let _ = write!(request, "Last-Event-ID: {last_event_id}\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write event stream request");
        let head = read_response_head(&mut stream).await;
        (stream, head)
    }
}

impl Drop for ManagementHarness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

async fn read_response_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        stream
            .read_exact(&mut byte)
            .await
            .expect("read event stream response headers");
        head.push(byte[0]);
    }
    head
}

async fn read_chunk(stream: &mut TcpStream) -> Vec<u8> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while !line.ends_with(b"\r\n") {
        stream
            .read_exact(&mut byte)
            .await
            .expect("read SSE chunk size");
        line.push(byte[0]);
    }
    let size = usize::from_str_radix(
        std::str::from_utf8(&line[..line.len() - 2])
            .expect("SSE chunk size")
            .trim(),
        16,
    )
    .expect("SSE chunk length");
    let mut body = vec![0_u8; size];
    stream.read_exact(&mut body).await.expect("read SSE chunk");
    let mut terminator = [0_u8; 2];
    stream
        .read_exact(&mut terminator)
        .await
        .expect("read SSE chunk terminator");
    assert_eq!(terminator, *b"\r\n");
    body
}
