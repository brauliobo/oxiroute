#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/rtmp.rs"]
mod rtmp_support;

use std::{
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
    Certificate, CertificateSource, Config, Listener, Management, Protocol, RtmpApplication,
    RtmpRecorderStart, RtmpService, render_lua,
};
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MediaSnapshot, RtmpApplication as RuntimeRtmpApplication,
    RtmpCapabilities, RtmpPushApplication, RtmpPushTarget, RtmpRegistry, RtmpRelayConfig,
    RtmpServiceRuntime, RtmpSessionPolicy, SessionId, StreamKey, TrackSnapshot,
    VideoCodecIdentifier,
};
use oxiroute_server::{
    HttpListenerApp, RtmpManagementApi, RuntimeMetrics, TopologySnapshot,
    config_coordinator::{
        CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigRevision, MAX_CANONICAL_CONFIG_BYTES,
    },
    runtime_plan,
};
use pingora::{
    apps::http_app::HttpServer,
    server::Fds,
    services::{Service as PingoraService, listening::Service as ListeningService},
};
use serde_json::Value;
use tempfile::TempDir;

use rtmp_support::RtmpSessionClient;
use tokio::{
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
                application: RtmpPushApplication::StreamName,
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

    assert_eq!(
        harness
            .request("GET", "/api/v1/config", None, None)
            .await
            .status,
        200
    );
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
                document.disk_revision.clone(),
                &token_path,
            )
            .is_ok()
    );

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        RtmpManagementApi::new(empty_registry(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator.clone(),
                document.disk_revision.clone(),
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
                document.disk_revision.clone(),
                &token_link,
            )
            .is_err()
    );

    fs::write(&token_path, format!("{TEST_TOKEN}\n\n")).unwrap();
    assert!(
        RtmpManagementApi::new(empty_registry(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator,
                document.disk_revision,
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
            Some(harness.active_revision.as_str()),
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
            Some(harness.active_revision.as_str()),
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
            Some(harness.active_revision.as_str()),
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
            Some(harness.active_revision.as_str()),
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
            Some(harness.active_revision.as_str()),
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
            Some(harness.active_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 200);
    let response_json = response.json();
    assert_eq!(response_json["outcome"], "unchanged_active");
    assert_eq!(response_json["activationState"], "active");
    assert_eq!(response_json["restartRequired"], false);
    assert_eq!(
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
            Some(harness.active_revision.as_str()),
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
        max_connections: Some(100),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    candidate.rtmp_services.push(RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        access_log: None,
        applications: vec![RtmpApplication {
            name: "broadcast".into(),
            live: true,
            idle_streams: false,
            push_targets: Vec::new(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            recorders: Vec::new(),
        }],
    });
    candidate
}

struct ManagementHarness {
    active_revision: ConfigRevision,
    address: SocketAddr,
    config_path: PathBuf,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    _directory: TempDir,
}

impl ManagementHarness {
    async fn start(config: &Config) -> Self {
        let directory = TempDir::new().expect("temporary canonical config directory");
        let config_path = directory.path().join("oxiroute.lua");
        fs::write(
            &config_path,
            render_lua(config).expect("active config renders"),
        )
        .expect("write active config");
        let coordinator = CanonicalConfigCoordinator::new(&config_path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("active config must load")
        };
        let active_revision = document.disk_revision.clone();
        let plan = runtime_plan(config).expect("active runtime plan");
        let metrics = RuntimeMetrics::new();
        metrics.set_rtmp_recording_supported(plan.rtmp_recording_supported);
        let api = RtmpManagementApi::new(empty_registry(), metrics, Arc::clone(&plan.topology))
            .with_config_coordinator(coordinator, active_revision.clone(), TEST_TOKEN)
            .expect("injected management token");

        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("reserve management listener");
        listener
            .set_nonblocking(true)
            .expect("make management listener nonblocking");
        let address = listener.local_addr().expect("management listener address");
        let http_server = HttpListenerApp::new(HttpServer::new_app(api), None);
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
            address,
            config_path,
            shutdown,
            task,
            _directory: directory,
        }
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
}

impl Drop for ManagementHarness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}
