#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn reports_truthful_empty_capabilities_when_ingest_is_disabled() {
    let api = management_api(empty_control(), RuntimeMetrics::new());

    let response = api.handle("GET", "/api/v1/rtmp/streams", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(body["revision"], "0");
    assert_eq!(body["capabilities"]["live_ingest"], false);
    assert_eq!(body["capabilities"]["manual_recording"], false);
    assert_eq!(body["streams"], serde_json::json!([]));
}

#[test]
pub(super) fn reports_bounded_rtmp_global_and_live_stats_without_stream_queries() {
    let runtime = basic_runtime();
    let service = runtime.service("live").expect("live service");
    let mut publisher = RtmpSessionClient::connect_handle(&service, "broadcast");
    publisher.publish("camera", 100);
    let mut audio = vec![0; 128];
    audio[..2].copy_from_slice(&[0xaf, 0x01]);
    publisher.publish_audio(1, &audio, 200);
    let api = management_api(runtime.control(), RuntimeMetrics::new());

    let global = api.handle("GET", "/api/v1/rtmp/stats/global", 300);
    let global: Value = serde_json::from_slice(&global.body).expect("global stats JSON");
    assert_eq!(global["global"]["activeStreams"], 1);
    assert_eq!(global["global"]["publishers"], 1);
    assert_eq!(global["global"]["audioPayloadBytes"], "128");

    let live = api.handle("GET", "/api/v1/rtmp/stats/live", 300);
    let live: Value = serde_json::from_slice(&live.body).expect("live stats JSON");
    assert_eq!(live["live"].as_array().map(Vec::len), Some(1));
    assert_eq!(live["live"][0]["application"], "broadcast");
    assert_eq!(live["live"][0]["name"], "camera");
    assert!(!live.to_string().contains('?'));
}

#[tokio::test]
pub(super) async fn queues_target_checked_publisher_disconnects_through_the_management_api() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(candidate_config(&active, "live"))
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let mut client = RtmpSessionClient::connect_handle(&harness.rtmp_runtime(), "broadcast");
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

#[tokio::test]
pub(super) async fn exposes_each_message_stream_in_client_stats() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(candidate_config(&active, "live"))
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let mut client = RtmpSessionClient::connect_handle(&harness.rtmp_runtime(), "broadcast");
    client.publish("camera-a", 100);

    let mut serializer = ChunkSerializer::new();
    send_raw_command(
        &mut client.server,
        &mut serializer,
        "createStream",
        0,
        Vec::new(),
        101,
    );
    send_raw_command(
        &mut client.server,
        &mut serializer,
        "publish",
        2,
        vec![
            Amf0Value::Utf8String("camera-b".into()),
            Amf0Value::Utf8String("live".into()),
        ],
        102,
    );

    let snapshot = client.server.client_snapshot().expect("client snapshot");
    assert_eq!(snapshot.message_streams.len(), 2);
    let stats = harness
        .request("GET", "/api/v1/rtmp/stats/clients", None, None)
        .await;
    assert_eq!(stats.status, 200);
    let body = stats.json();
    let streams = body["clients"][0]["messageStreams"]
        .as_array()
        .expect("message stream array");
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0]["messageStreamId"], 1);
    assert_eq!(streams[1]["messageStreamId"], 2);
    assert_eq!(streams[0]["role"], "publisher");
    assert_eq!(streams[1]["role"], "publisher");
}

#[test]
pub(super) fn reports_http3_only_when_a_listener_is_active() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("reverse-h3", "http3", "127.0.0.1:9443", Some(64))
        .expect("HTTP/3 listener metrics");
    listener.mark_listening();
    let api = management_api(empty_control(), metrics);

    let response = api.handle("GET", "/api/v1/capabilities", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(body["http3"]["reverse"]["status"], "active");
    assert_eq!(
        body["http3"]["reverse"]["listeners"],
        serde_json::json!(["reverse-h3"])
    );
    assert_eq!(body["http3"]["reverse"]["fallback"], "none");
    assert_eq!(body["http3"]["forward"]["status"], "unconfigured");
}

#[test]
pub(super) fn status_reports_component_health_generation_age_and_certificate_sections() {
    let metrics = RuntimeMetrics::new();
    let api = management_api(empty_control(), metrics);

    let response = api.handle("GET", "/api/v1/status", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("status JSON");

    assert_eq!(response.status, 200);
    assert_eq!(body["components"]["process"]["state"], "healthy");
    assert_eq!(body["components"]["host"]["state"], "healthy");
    assert_eq!(
        body["components"]["generation"]["reason"],
        "active_generation_unavailable"
    );
    assert!(body["activeGenerationAgeMs"].as_u64().is_some());
    assert_eq!(body["certificates"]["certbot"], serde_json::json!([]));
    assert_eq!(body["certificates"]["acmeManaged"], serde_json::json!([]));
    assert_eq!(body["certificates"]["directFiles"], serde_json::json!([]));
}

#[tokio::test]
pub(super) async fn serves_authenticated_vod_objects_and_single_ranges() {
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
    let harness = ManagementHarness::start(
        &(config.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
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
        &[
            ("Authorization", authorization.as_str()),
            ("Range", "bytes=2-5"),
        ],
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
pub(super) async fn serves_authenticated_dash_manifests_segments_and_single_ranges() {
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
    let harness = ManagementHarness::start(
        &(config.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let mut publisher = RtmpSessionClient::connect_handle(&harness.rtmp_runtime(), "broadcast");
    publisher.publish("camera", 1_000);
    publisher.publish_audio(0, &[0xaf, 0, 0x12, 0x10], 1_001);
    publisher.publish_video(
        0,
        &[
            0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1, 0, 2,
            0x68, 0xce,
        ],
        1_002,
    );
    publisher.publish_video(0, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 1], 1_003);
    publisher.publish_audio(0, &[0xaf, 1, 2, 3, 4], 1_004);
    publisher.publish_video(1_000, &[0x27, 1, 0, 0, 0, 0, 0, 0, 2, 0x41, 2], 1_005);
    publisher.publish_video(2_000, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 3], 1_006);

    let manifest_path = "/api/v1/rtmp/media/live/broadcast/camera/dash/manifest.mpd";
    let deadline = Instant::now() + Duration::from_secs(30);
    let manifest = loop {
        let response = harness.request("GET", manifest_path, None, None).await;
        if response.status == 200 {
            break response;
        }
        assert!(
            Instant::now() < deadline,
            "DASH manifest publication timeout"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(
        manifest.header("content-type"),
        Some("application/dash+xml")
    );
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
        &[
            ("Authorization", authorization.as_str()),
            ("Range", "bytes=0-7"),
        ],
        &[],
    )
    .await;
    assert_eq!(ranged.status, 206);
    assert_eq!(ranged.body().len(), 8);
    assert!(
        ranged
            .header("content-range")
            .is_some_and(|value| value.starts_with("bytes 0-7/"))
    );
}

#[test]
pub(super) fn exposes_enhanced_video_codec_identity_with_recording_support() {
    let runtime = basic_runtime();
    let service = runtime.service("live").expect("live service");
    let mut publisher = RtmpSessionClient::connect_handle(&service, "broadcast");
    publisher.publish("hevc-camera", 100);
    let mut payload = vec![0x91, b'h', b'v', b'c', b'1'];
    payload.resize(4_096, 0x55);
    publisher.publish_video(1, &payload, 200);
    let api = management_api(runtime.control(), RuntimeMetrics::new());

    let response = api.handle("GET", "/api/v1/rtmp/streams", 300);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");
    let video = &body["streams"][0]["media"]["video"];
    assert_eq!(
        video,
        &serde_json::json!({
            "codec_id": null,
            "codec_fourcc": "hvc1",
            "codec_name": "hevc",
            "recording_supported": true,
            "payload_bytes": "4096",
            "last_rtmp_timestamp_ms": 1,
            "last_observed_at_unix_ms": 200,
        })
    );
}

#[test]
pub(super) fn serves_prebuilt_ui_assets_without_request_time_filesystem_paths() {
    let directory = tempfile::tempdir().expect("temporary UI directory");
    fs::create_dir(directory.path().join("assets")).expect("asset directory");
    fs::write(
        directory.path().join("index.html"),
        "<main>Broadcast desk</main>",
    )
    .expect("index asset");
    fs::write(directory.path().join("assets/app.css"), "body{color:white}").expect("CSS asset");
    let api = RtmpManagementApi::with_ui_dir(
        empty_control(),
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
pub(super) fn exposes_runtime_listener_and_rtmp_monitoring() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("live", "rtmp", "127.0.0.1:1935", 100)
        .expect("listener metrics");
    let connection = listener.begin_connection().expect("active connection");
    connection
        .record_bytes_received(4_096)
        .expect("received traffic");
    connection.record_bytes_sent(3_073).expect("sent traffic");
    let runtime = basic_runtime();
    let service = runtime.service("live").expect("live service");
    let mut publisher = RtmpSessionClient::connect_handle(&service, "broadcast");
    publisher.publish("camera", 100);
    let mut audio = vec![0; 1_024];
    audio[..2].copy_from_slice(&[0xaf, 0x01]);
    publisher.publish_audio(1, &audio, 200);
    let mut video = vec![0; 8_192];
    video[..5].copy_from_slice(&[0x17, 0x01, 0x00, 0x00, 0x00]);
    publisher.publish_video(2, &video, 201);
    let api = management_api(runtime.control(), metrics);

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
pub(super) fn monitoring_exposes_fixed_rtmp_access_log_metrics_without_sink_identity() {
    let api = management_api(empty_control(), RuntimeMetrics::new());
    let response = api.handle("GET", "/api/v1/monitoring", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(body["rtmp"]["accessLog"]["queueCapacity"], 1_024);
    assert_eq!(body["rtmp"]["accessLog"]["queueDepth"], "0");
    assert_eq!(body["rtmp"]["accessLog"]["dropped"], "0");
    assert!(body["rtmp"]["accessLog"].get("path").is_none());
    assert!(body["rtmp"]["accessLog"].get("service").is_none());
}

#[test]
pub(super) fn relay_state_and_counters_are_observable_without_stream_queries() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve absent relay port");
    let destination = listener.local_addr().expect("relay destination");
    drop(listener);
    let mut config = candidate_config(&editable_config(), "live");
    config.rtmp_services[0].outbound_policy.deny_private = false;
    config.rtmp_services[0].applications[0]
        .relay
        .push_reconnect_ms = 20;
    config.rtmp_services[0].applications[0]
        .relay
        .connect_timeout_ms = 10;
    config.rtmp_services[0].applications[0]
        .push_targets
        .push(oxiroute_config::RtmpPushTarget {
            host: destination.ip().to_string(),
            port: destination.port(),
            application: "$name".into(),
            scheme: oxiroute_config::RtmpTransport::Rtmp,
            stream_name: None,
            tc_url: None,
            flash_version: None,
            credentials: None,
        });
    let runtime = runtime_for_config(config);
    let service = runtime.service("live").expect("live service");
    let mut publisher = RtmpSessionClient::connect_handle(&service, "broadcast");
    publisher.publish("camera?token=relay-wire-secret", 100);
    publisher.publish_audio(1, &[0xaf, 0x01, 0x44], 101);
    let deadline = Instant::now() + Duration::from_secs(30);
    let control = runtime.control();
    while control.catalog_snapshot().streams[0].relays[0]
        .status
        .connection_attempts
        < 2
    {
        assert!(Instant::now() < deadline, "relay retry timeout");
        thread::sleep(Duration::from_millis(5));
    }
    let api = management_api(control, RuntimeMetrics::new());

    let catalog = api.handle("GET", "/api/v1/rtmp/streams", 200);
    let monitoring = api.handle("GET", "/api/v1/monitoring", 200);
    let catalog_json: Value = serde_json::from_slice(&catalog.body).expect("catalog JSON");
    let monitoring_json: Value = serde_json::from_slice(&monitoring.body).expect("monitoring JSON");
    assert_eq!(catalog_json["streams"][0]["relays"][0]["phase"], "backoff");
    assert_eq!(
        catalog_json["streams"][0]["relays"][0]["destination"]["application"],
        "camera"
    );
    assert_eq!(
        catalog_json["streams"][0]["relays"][0]["dns_refresh_attempts"],
        "0"
    );
    assert_eq!(
        catalog_json["streams"][0]["relays"][0]["dns_refresh_failures"],
        "0"
    );
    assert!(
        monitoring_json["rtmp"]["relayConnectionAttempts"]
            .as_str()
            .is_some_and(|attempts| attempts.parse::<u64>().is_ok_and(|attempts| attempts >= 2))
    );
    assert_eq!(monitoring_json["rtmp"]["relayDnsRefreshAttempts"], "0");
    assert_eq!(monitoring_json["rtmp"]["relayDnsRefreshFailures"], "0");
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
pub(super) fn monitoring_response_serializes_rtmp_cumulative_totals_as_decimal_strings() {
    let runtime = basic_runtime();
    let service = runtime.service("live").expect("live service");
    let mut publisher = RtmpSessionClient::connect_handle(&service, "broadcast");
    publisher.publish("counter", 100);
    publisher.publish_audio(1, &[0xaf, 0x01, 0x44], 200);
    let api = management_api(runtime.control(), RuntimeMetrics::new());

    let response = api.handle("GET", "/api/v1/monitoring", 300);
    let body: Value = serde_json::from_slice(&response.body).expect("JSON response");

    assert_eq!(response.status, 200);
    assert_eq!(body["rtmp"]["mediaPayloadBytesReceived"], "3");
}
