#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[allow(dead_code)]
#[path = "support/process.rs"]
mod process_support;
#[path = "support/rtmp.rs"]
mod rtmp_support;

use std::{
    fs,
    net::SocketAddr,
    path::Path,
    time::{Duration, Instant},
};

use oxiroute_config::{
    Config, HttpPathSelector, HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy,
    L4Service, Listener, Management, Protocol, RtmpApplication, RtmpRecorderStart, RtmpService,
    Stats, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
};
use rml_rtmp::sessions::ClientSessionEvent;
use rustix::fs::{FlockOperation, flock};
use serde_json::Value;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::{sleep, timeout},
};

use config_support::{empty_config, rtmp_recorder_with_queue_bytes, socket_bind};
use fixture_support::create_secure_root;
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_config};
use rtmp_support::RtmpWireClient;

const TOKEN: &str = "55f17e0e05826acaa3bc493350f59986f12d42ad762ddf934570c51fd28bea74";
const WIRE_TIMEOUT: Duration = Duration::from_secs(10);
const INITIAL_PLAYBACK_TICKS: Duration = Duration::from_millis(30);

#[tokio::test]
async fn idle_and_publisher_connections_survive_initial_playback_timer_ticks() {
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = idle_runtime_config(management_address, rtmp_address);
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let mut publisher =
        RtmpWireClient::connect_after(rtmp_address, "live", INITIAL_PLAYBACK_TICKS).await;
    sleep(INITIAL_PLAYBACK_TICKS).await;
    publisher.publish("timer-regression").await;
    sleep(INITIAL_PLAYBACK_TICKS).await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;

    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "live").is_some_and(|stream| {
            stream["media"]["audio"]["payload_bytes"] == "6" && stream["publisher"].is_object()
        })
    })
    .await;
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = http_request(
        management_address,
        "GET",
        "/api/v1/status",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();
    config.max_connections = Some(100);
    write_config(&server.config_path, &config);
    timeout(WIRE_TIMEOUT, async {
        loop {
            let status = http_request(
                management_address,
                "GET",
                "/api/v1/status",
                &[("Authorization", &authorization)],
                &[],
            )
            .await;
            if status.status == 200
                && status.json()["activeRevision"].as_str() != Some(original_revision.as_str())
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("generation reload timed out");
    publisher.publish_audio(3, &[0xaf, 0x01, 0x55]).await;
    publisher.close().await;
    server.shutdown();
}

#[tokio::test]
async fn queued_connections_survive_generation_handoff_on_every_listener_kind() {
    const CONNECTIONS_PER_ENDPOINT: usize = 24;

    let management_address = reserve_tcp_address();
    let stats_addresses = [reserve_tcp_address(), reserve_tcp_address()];
    let http_addresses = [reserve_tcp_address(), reserve_tcp_address()];
    let tcp_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let upstream = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("TCP upstream bind");
    let upstream_address = upstream.local_addr().expect("TCP upstream address");
    let upstream_task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = upstream.accept().await.expect("TCP upstream accept");
            tokio::spawn(async move {
                let mut payload = [0; 4];
                if stream.read_exact(&mut payload).await.is_err() {
                    return;
                }
                stream
                    .write_all(&payload)
                    .await
                    .expect("TCP upstream write");
            });
        }
    });
    let mut config = handoff_runtime_config(
        management_address,
        stats_addresses,
        http_addresses,
        tcp_address,
        rtmp_address,
        upstream_address,
    );
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    for address in [
        management_address,
        stats_addresses[0],
        stats_addresses[1],
        http_addresses[0],
        http_addresses[1],
        tcp_address,
        rtmp_address,
    ] {
        server.wait_for_tcp(address).await;
    }

    let management = connect_many(management_address, CONNECTIONS_PER_ENDPOINT).await;
    let first_stats = connect_many(stats_addresses[0], CONNECTIONS_PER_ENDPOINT).await;
    let second_stats = connect_many(stats_addresses[1], CONNECTIONS_PER_ENDPOINT).await;
    let first_http = connect_many(http_addresses[0], CONNECTIONS_PER_ENDPOINT).await;
    let second_http = connect_many(http_addresses[1], CONNECTIONS_PER_ENDPOINT).await;
    let tcp = connect_many(tcp_address, CONNECTIONS_PER_ENDPOINT).await;
    let rtmp = connect_many(rtmp_address, CONNECTIONS_PER_ENDPOINT).await;

    let authorization = format!("Bearer {TOKEN}");
    let original_revision = http_request(
        management_address,
        "GET",
        "/api/v1/status",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();
    config.max_connections = Some(1_024);
    write_config(&server.config_path, &config);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    let ready_request = b"GET /ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let fixed_request = b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    assert_http_connections(management, ready_request, b"200 OK").await;
    assert_http_connections(first_stats, ready_request, b"200 OK").await;
    assert_http_connections(second_stats, ready_request, b"200 OK").await;
    assert_http_connections(first_http, fixed_request, b"handoff-ok").await;
    assert_http_connections(second_http, fixed_request, b"handoff-ok").await;
    assert_tcp_connections(tcp).await;
    assert_rtmp_connections(rtmp).await;

    upstream_task.abort();
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn built_runtime_publishes_plays_and_records_continuous_and_manual_streams_over_tcp() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "continuous-private-root");
    let manual_root = create_secure_root(recording_directory.path(), "manual-private-root");

    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let authorization = format!("Bearer {TOKEN}");

    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher
        .publish("camera?token=continuous-wire-secret")
        .await;
    let mut viewer = RtmpWireClient::connect(rtmp_address, "continuous").await;
    viewer.play("camera?viewer=browser-wire-secret").await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;
    let playback = viewer
        .wait_for_event(Vec::new(), |event| {
            matches!(
                event,
                ClientSessionEvent::AudioDataReceived { data, .. }
                    if data.as_ref() == [0xaf, 0x01, 0x44]
            )
        })
        .await;
    assert!(matches!(
        playback,
        ClientSessionEvent::AudioDataReceived { timestamp, .. } if timestamp.value == 2
    ));

    let continuous_catalog = wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["subscriber_count"] == 1
                && stream["recorders"][0]["phase"]["state"] == "recording"
                && stream["recorders"][0]["bytes_written"] != "0"
        })
    })
    .await;
    let continuous_wire = continuous_catalog.to_string();
    assert!(!continuous_wire.contains(TOKEN));
    assert!(!continuous_wire.contains("continuous-wire-secret"));
    assert!(!continuous_wire.contains("browser-wire-secret"));
    assert!(!continuous_wire.contains(&continuous_root.display().to_string()));
    assert_eq!(
        stream_for(&continuous_catalog, "continuous").expect("continuous stream")["name"],
        "camera"
    );

    viewer.close().await;
    publisher.close().await;
    wait_for_file(&continuous_root.join("camera.flv")).await;

    let mut manual_publisher = RtmpWireClient::connect(rtmp_address, "manual").await;
    manual_publisher
        .publish("operator?token=manual-wire-secret")
        .await;
    let manual_catalog = wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "manual").is_some()
    })
    .await;
    let manual_stream = stream_for(&manual_catalog, "manual").expect("manual stream");
    let stream_id = manual_stream["id"].as_str().expect("manual stream ID");
    let recorder_id = manual_stream["recorders"][0]["id"]
        .as_str()
        .expect("manual recorder ID");
    let active_revision = http_request(
        management_address,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();
    let start_path = format!("/api/v1/rtmp/streams/{stream_id}/recorders/{recorder_id}/start");
    let started = http_request(
        management_address,
        "POST",
        &start_path,
        &[
            ("Authorization", &authorization),
            ("If-Generation-Revision", &active_revision),
        ],
        &[],
    )
    .await;
    assert!(matches!(started.status, 200 | 202));
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "manual")
            .is_some_and(|stream| stream["recorders"][0]["phase"]["state"] == "recording")
    })
    .await;

    manual_publisher.publish_audio(3, &[0xaf, 0x00, 0x12]).await;
    manual_publisher.publish_audio(4, &[0xaf, 0x01, 0x77]).await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "manual").is_some_and(|stream| {
            stream["recorders"][0]["bytes_written"]
                .as_str()
                .and_then(|bytes| bytes.parse::<u64>().ok())
                .is_some_and(|bytes| bytes > 13)
        })
    })
    .await;
    let stop_path = format!("/api/v1/rtmp/streams/{stream_id}/recorders/{recorder_id}/stop");
    let stopped = http_request(
        management_address,
        "POST",
        &stop_path,
        &[
            ("Authorization", &authorization),
            ("If-Generation-Revision", &active_revision),
        ],
        &[],
    )
    .await;
    assert!(matches!(stopped.status, 200 | 202));
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "manual")
            .is_some_and(|stream| stream["recorders"][0]["phase"]["state"] == "idle")
    })
    .await;
    wait_for_file(&manual_root.join("operator.flv")).await;

    let monitoring = http_request(
        management_address,
        "GET",
        "/api/v1/monitoring",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(monitoring.status, 200);
    let monitoring_wire = String::from_utf8(monitoring.body).expect("monitoring UTF-8");
    assert!(!monitoring_wire.contains(TOKEN));
    assert!(!monitoring_wire.contains("manual-wire-secret"));
    assert!(!monitoring_wire.contains(&manual_root.display().to_string()));

    manual_publisher.close().await;
    server.shutdown();
}

#[tokio::test]
async fn phoenix_continuous_recording_resumes_after_process_restart_and_publisher_reconnect() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "phoenix-recordings");
    let manual_root = create_secure_root(recording_directory.path(), "unused-manual-recordings");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    let recorder = &mut config.rtmp_services[0].applications[0].recorders[0];
    recorder.suffix_template = "-%Y%m%d_%H%M%S.mp4".into();
    recorder.append_unix_seconds = true;
    recorder.timezone = oxiroute_config::RtmpRecorderTimezone::Iana("America/Bahia".into());
    recorder.time_basis = oxiroute_config::RtmpRecorderTimeBasis::SegmentStart;
    recorder.segment_naming = oxiroute_config::RtmpRecorderSegmentNaming::NginxCompatible;
    recorder.rotation_interval_ms = Some(3_600_000);

    let mut first_server = ServerProcess::start(&config, Some(TOKEN));
    first_server.wait_for_tcp(management_address).await;
    first_server.wait_for_tcp(rtmp_address).await;
    let mut first_publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    first_publisher.publish("camera").await;
    first_publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    first_publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["recorders"][0]["phase"]["state"] == "recording"
                && stream["recorders"][0]["bytes_written"]
                    .as_str()
                    .and_then(|bytes| bytes.parse::<u64>().ok())
                    .is_some_and(|bytes| bytes > 13)
        })
    })
    .await;

    first_server.shutdown();
    drop(first_publisher);
    let first_files = wait_for_recording_file_count(&continuous_root, 1).await;
    assert_recording_extensions(&first_files, "mp4");
    let first_path = first_files[0].clone();
    let first_bytes = fs::read(&first_path).expect("first Phoenix-shaped segment");
    assert!(first_bytes.starts_with(b"FLV"));

    let mut second_server = ServerProcess::start(&config, Some(TOKEN));
    second_server.wait_for_tcp(management_address).await;
    second_server.wait_for_tcp(rtmp_address).await;
    let mut second_publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    second_publisher.publish("camera").await;
    second_publisher.publish_audio(3, &[0xaf, 0x00, 0x12]).await;
    second_publisher.publish_audio(4, &[0xaf, 0x01, 0x55]).await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["recorders"][0]["phase"]["state"] == "recording"
                && stream["recorders"][0]["bytes_written"] != "0"
        })
    })
    .await;
    second_publisher.close().await;

    let resumed_files = wait_for_recording_file_count(&continuous_root, 1).await;
    assert_recording_extensions(&resumed_files, "mp4");
    assert_eq!(resumed_files, vec![first_path.clone()]);
    let resumed_bytes = fs::read(first_path).expect("resumed Phoenix-shaped segment");
    assert!(resumed_bytes.starts_with(b"FLV"));
    assert!(resumed_bytes.len() > first_bytes.len());
    second_server.shutdown();
}

#[tokio::test]
async fn process_shutdown_waits_for_stalled_recorder_cleanup_without_exceeding_the_budget() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "stalled-recordings");
    let manual_root = create_secure_root(recording_directory.path(), "unused-manual-recordings");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let ownership = fs::File::open(&continuous_root).expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("camera").await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;
    sleep(Duration::from_millis(50)).await;
    publisher.close().await;
    sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    server.shutdown();
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(500),
        "process exited before recorder cleanup was bounded: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "recorder cleanup exceeded the process budget: {elapsed:?}"
    );
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
}

#[tokio::test]
async fn evicted_generation_keeps_recording_live_connections_until_final_shutdown() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "previous-recordings");
    let manual_root = create_secure_root(recording_directory.path(), "unused-manual-recordings");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    config.rtmp_services[0].applications[0].recorders[0].shutdown_timeout_ms = 3_000;
    config.rtmp_services[0].applications[1].recorders[0].start = RtmpRecorderStart::Continuous;
    config.rtmp_services[0].applications[1].recorders[0].shutdown_timeout_ms = 30_000;
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;
    let ownership = fs::File::open(&manual_root).expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall second recorder");
    let mut stalled = RtmpWireClient::connect(rtmp_address, "manual").await;
    stalled.publish("blocked").await;
    stalled.publish_audio(1, &[0xaf, 0x01, 0x33]).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("camera").await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;
    let initial_size = wait_for_recording_growth(&continuous_root, 0).await;

    config.max_connections = Some(64);
    write_config(&server.config_path, &config);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;
    publisher.publish_audio(3, &[0xaf, 0x01, 0x55]).await;
    let second_size = wait_for_recording_growth(&continuous_root, initial_size).await;
    let second_revision = active_revision(management_address, &authorization).await;
    config.max_connections = Some(65);
    write_config(&server.config_path, &config);
    wait_for_new_revision(management_address, &authorization, &second_revision).await;
    publisher.publish_audio(4, &[0xaf, 0x01, 0x66]).await;
    wait_for_recording_growth(&continuous_root, second_size).await;
    publisher.close().await;
    let files = wait_for_recording_file_count(&continuous_root, 1).await;
    let recording_path = files[0].clone();
    let recording = timeout(WIRE_TIMEOUT, async {
        loop {
            let recording = fs::read(&recording_path).expect("evicted generation recording");
            if recording
                .windows(3)
                .any(|bytes| bytes == [0xaf, 0x01, 0x66])
            {
                return recording;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("evicted generation recording did not drain");
    assert!(
        recording
            .windows(3)
            .any(|bytes| bytes == [0xaf, 0x01, 0x55])
    );
    assert!(
        recording
            .windows(3)
            .any(|bytes| bytes == [0xaf, 0x01, 0x66])
    );
    stalled.close().await;
    sleep(Duration::from_millis(150)).await;

    let shutdown = tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        server.shutdown();
        started.elapsed()
    });
    sleep(Duration::from_millis(250)).await;
    assert!(
        !shutdown.is_finished(),
        "process shutdown did not retain evicted recorder cleanup"
    );
    let elapsed = timeout(WIRE_TIMEOUT, shutdown)
        .await
        .expect("process shutdown timeout")
        .expect("process shutdown task");
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");

    assert!(
        elapsed >= Duration::from_secs(4),
        "process did not apply its deadline to evicted recorder cleanup: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "evicted generation recorder exceeded the process budget: {elapsed:?}"
    );
}

fn runtime_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    continuous_root: &Path,
    manual_root: &Path,
) -> Config {
    Config {
        management: Some(Management {
            bind: management_address,
            ui_dir: None,
        }),
        stats: None,
        listeners: vec![Listener {
            name: "wire-rtmp".into(),
            bind: socket_bind(rtmp_address),
            protocol: Protocol::Rtmp,
            service: Some("wire-rtmp".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
        rtmp_services: vec![RtmpService {
            name: "wire-rtmp".into(),
            outbound_chunk_size: 4_096,
            access_log: None,
            outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
            exec_profiles: Vec::new(),
            applications: vec![
                application("continuous", RtmpRecorderStart::Continuous, continuous_root),
                application("manual", RtmpRecorderStart::Manual, manual_root),
            ],
        }],
        ..empty_config()
    }
}

fn idle_runtime_config(management_address: SocketAddr, rtmp_address: SocketAddr) -> Config {
    Config {
        management: Some(Management {
            bind: management_address,
            ui_dir: None,
        }),
        stats: None,
        listeners: vec![Listener {
            name: "timer-rtmp".into(),
            bind: socket_bind(rtmp_address),
            protocol: Protocol::Rtmp,
            service: Some("timer-rtmp".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(4),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
        rtmp_services: vec![RtmpService {
            name: "timer-rtmp".into(),
            outbound_chunk_size: 4_096,
            access_log: None,
            outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
            exec_profiles: Vec::new(),
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: true,
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
        }],
        ..empty_config()
    }
}

#[allow(clippy::too_many_lines)]
fn handoff_runtime_config(
    management_address: SocketAddr,
    stats_addresses: [SocketAddr; 2],
    http_addresses: [SocketAddr; 2],
    tcp_address: SocketAddr,
    rtmp_address: SocketAddr,
    upstream_address: SocketAddr,
) -> Config {
    let listener = |name: &str, address, protocol, service: &str| Listener {
        name: name.into(),
        bind: socket_bind(address),
        protocol,
        service: Some(service.into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(512),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    };
    Config {
        management: Some(Management {
            bind: management_address,
            ui_dir: None,
        }),
        stats: Some(Stats {
            binds: stats_addresses.to_vec(),
            admin_token_file: None,
            pages: Vec::new(),
        }),
        listeners: vec![
            listener(
                "handoff-http-a",
                http_addresses[0],
                Protocol::Http,
                "handoff-http",
            ),
            listener(
                "handoff-http-b",
                http_addresses[1],
                Protocol::Http,
                "handoff-http",
            ),
            listener("handoff-tcp", tcp_address, Protocol::Tcp, "handoff-tcp"),
            listener("handoff-rtmp", rtmp_address, Protocol::Rtmp, "handoff-rtmp"),
        ],
        upstream_pools: vec![UpstreamPool {
            name: "handoff-upstream".into(),
            servers: Vec::new(),
            endpoints: vec![UpstreamEndpoint::Socket {
                address: upstream_address,
            }],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Safe,
        }],
        http_services: vec![HttpService {
            name: "handoff-http".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: HttpRouteAction::FixedResponse {
                    status: 200,
                    body: "handoff-ok".into(),
                    headers: Vec::new(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 1_000,
            max_request_body_bytes: Some(1_024),
            gzip: None,
            access_log: None,
        }],
        rtmp_services: vec![RtmpService {
            name: "handoff-rtmp".into(),
            outbound_chunk_size: 4_096,
            access_log: None,
            outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
            exec_profiles: Vec::new(),
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: true,
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
        }],
        l4_services: vec![L4Service {
            name: "handoff-tcp".into(),
            upstream_pool: "handoff-upstream".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 10_000,
            lifetime_timeout_ms: None,
            proxy_protocol: None,
            udp: None,
        }],
        ..empty_config()
    }
}

async fn connect_many(address: SocketAddr, count: usize) -> Vec<TcpStream> {
    let mut connections = JoinSet::new();
    for _ in 0..count {
        connections.spawn(async move {
            TcpStream::connect(address)
                .await
                .unwrap_or_else(|error| panic!("connect to {address}: {error}"))
        });
    }
    let mut streams = Vec::with_capacity(count);
    while let Some(stream) = connections.join_next().await {
        streams.push(stream.expect("connection task"));
    }
    streams
}

async fn wait_for_new_revision(address: SocketAddr, authorization: &str, original: &str) {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let status = http_request(
                address,
                "GET",
                "/api/v1/status",
                &[("Authorization", authorization)],
                &[],
            )
            .await;
            assert_eq!(status.status, 200);
            if status.json()["activeRevision"].as_str() != Some(original) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("generation reload timed out");
}

async fn active_revision(address: SocketAddr, authorization: &str) -> String {
    http_request(
        address,
        "GET",
        "/api/v1/status",
        &[("Authorization", authorization)],
        &[],
    )
    .await
    .json()["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned()
}

async fn assert_http_connections(
    streams: Vec<TcpStream>,
    request: &'static [u8],
    expected: &'static [u8],
) {
    let mut exchanges = JoinSet::new();
    for mut stream in streams {
        exchanges.spawn(async move {
            stream.write_all(request).await.expect("HTTP request write");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .await
                .expect("HTTP response read");
            assert!(
                response
                    .windows(expected.len())
                    .any(|bytes| bytes == expected),
                "unexpected HTTP response: {}",
                String::from_utf8_lossy(&response)
            );
        });
    }
    timeout(WIRE_TIMEOUT, async {
        while let Some(exchange) = exchanges.join_next().await {
            exchange.expect("HTTP exchange task");
        }
    })
    .await
    .expect("HTTP handoff exchanges timed out");
}

async fn assert_tcp_connections(streams: Vec<TcpStream>) {
    let mut exchanges = JoinSet::new();
    for (index, mut stream) in streams.into_iter().enumerate() {
        exchanges.spawn(async move {
            let payload = u32::try_from(index)
                .expect("TCP payload index")
                .to_be_bytes();
            stream.write_all(&payload).await.expect("TCP relay write");
            let mut response = [0; 4];
            stream
                .read_exact(&mut response)
                .await
                .expect("TCP relay read");
            assert_eq!(response, payload);
        });
    }
    timeout(WIRE_TIMEOUT, async {
        while let Some(exchange) = exchanges.join_next().await {
            exchange.expect("TCP exchange task");
        }
    })
    .await
    .expect("TCP handoff exchanges timed out");
}

async fn assert_rtmp_connections(streams: Vec<TcpStream>) {
    let mut handshakes = JoinSet::new();
    for stream in streams {
        handshakes.spawn(async move {
            RtmpWireClient::establish(stream, "live")
                .await
                .close()
                .await;
        });
    }
    timeout(WIRE_TIMEOUT, async {
        while let Some(handshake) = handshakes.join_next().await {
            handshake.expect("RTMP handshake task");
        }
    })
    .await
    .expect("RTMP handoff handshakes timed out");
}

fn application(name: &str, start: RtmpRecorderStart, root_directory: &Path) -> RtmpApplication {
    RtmpApplication {
        name: name.into(),
        live: true,
        idle_streams: true,
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
        recorders: vec![rtmp_recorder_with_queue_bytes(
            "archive",
            start,
            root_directory,
            1024 * 1024,
        )],
    }
}

async fn wait_for_catalog(
    management_address: SocketAddr,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let authorization = format!("Bearer {TOKEN}");
            let response = http_request(
                management_address,
                "GET",
                "/api/v1/rtmp/streams",
                &[("Authorization", &authorization)],
                &[],
            )
            .await;
            assert_eq!(response.status, 200);
            let catalog = response.json();
            if predicate(&catalog) {
                return catalog;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("RTMP catalog condition timed out")
}

fn stream_for<'a>(catalog: &'a Value, application: &str) -> Option<&'a Value> {
    catalog["streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["application"] == application)
}

fn assert_recording_extensions(paths: &[std::path::PathBuf], expected: &str) {
    for path in paths {
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some(expected),
            "unexpected recording path {}",
            path.display()
        );
    }
}

async fn wait_for_file(path: &Path) {
    timeout(WIRE_TIMEOUT, async {
        loop {
            if fs::metadata(path).is_ok_and(|metadata| metadata.len() > 13) {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("recording did not finalize at {}", path.display()));
}

async fn wait_for_recording_growth(root: &Path, previous_size: u64) -> u64 {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let size = fs::read_dir(root)
                .expect("recording directory")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_name().to_str().is_some_and(|name| {
                        name != ".oxiroute-recording.lock" && !name.starts_with('.')
                    })
                })
                .filter_map(|entry| entry.metadata().ok())
                .map(|metadata| metadata.len())
                .max()
                .unwrap_or(0);
            if size > previous_size {
                return size;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording did not grow")
}

async fn wait_for_recording_file_count(root: &Path, expected: usize) -> Vec<std::path::PathBuf> {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let mut files = fs::read_dir(root)
                .expect("recording directory")
                .map(|entry| entry.expect("recording entry").path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name != ".oxiroute-recording.lock" && !name.ends_with(".partial")
                            })
                })
                .collect::<Vec<_>>();
            files.sort();
            if files.len() == expected
                && files
                    .iter()
                    .all(|path| fs::metadata(path).is_ok_and(|metadata| metadata.len() > 13))
            {
                return files;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording file count")
}
