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

use std::{fs, net::SocketAddr, path::Path, time::Duration};

use oxiroute_config::{
    Config, Listener, Management, Protocol, RtmpApplication, RtmpRecorderStart, RtmpService,
};
use rml_rtmp::sessions::ClientSessionEvent;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

use config_support::{empty_config, rtmp_recorder_with_queue_bytes, socket_bind};
use fixture_support::create_secure_root;
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address};
use rtmp_support::RtmpWireClient;

const TOKEN: &str = "55f17e0e05826acaa3bc493350f59986f12d42ad762ddf934570c51fd28bea74";
const WIRE_TIMEOUT: Duration = Duration::from_secs(10);
const INITIAL_PLAYBACK_TICKS: Duration = Duration::from_millis(30);

#[tokio::test]
async fn idle_and_publisher_connections_survive_initial_playback_timer_ticks() {
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = idle_runtime_config(management_address, rtmp_address);
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
    publisher.close().await;
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
    let start_path = format!("/api/v1/rtmp/streams/{stream_id}/recorders/{recorder_id}/start");
    let started = http_request(management_address, "POST", &start_path, &[], &[]).await;
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
    let stopped = http_request(management_address, "POST", &stop_path, &[], &[]).await;
    assert!(matches!(stopped.status, 200 | 202));
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "manual")
            .is_some_and(|stream| stream["recorders"][0]["phase"]["state"] == "idle")
    })
    .await;
    wait_for_file(&manual_root.join("operator.flv")).await;

    let monitoring = http_request(management_address, "GET", "/api/v1/monitoring", &[], &[]).await;
    assert_eq!(monitoring.status, 200);
    let monitoring_wire = String::from_utf8(monitoring.body).expect("monitoring UTF-8");
    assert!(!monitoring_wire.contains(TOKEN));
    assert!(!monitoring_wire.contains("manual-wire-secret"));
    assert!(!monitoring_wire.contains(&manual_root.display().to_string()));

    manual_publisher.close().await;
    server.shutdown();
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
        listeners: vec![Listener {
            name: "wire-rtmp".into(),
            bind: socket_bind(rtmp_address),
            protocol: Protocol::Rtmp,
            service: Some("wire-rtmp".into()),
            tls_profile: None,
            max_connections: Some(8),
        }],
        rtmp_services: vec![RtmpService {
            name: "wire-rtmp".into(),
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
        listeners: vec![Listener {
            name: "timer-rtmp".into(),
            bind: socket_bind(rtmp_address),
            protocol: Protocol::Rtmp,
            service: Some("timer-rtmp".into()),
            tls_profile: None,
            max_connections: Some(4),
        }],
        rtmp_services: vec![RtmpService {
            name: "timer-rtmp".into(),
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: true,
                recorders: Vec::new(),
            }],
        }],
        ..empty_config()
    }
}

fn application(name: &str, start: RtmpRecorderStart, root_directory: &Path) -> RtmpApplication {
    RtmpApplication {
        name: name.into(),
        live: true,
        idle_streams: true,
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
            let response =
                http_request(management_address, "GET", "/api/v1/rtmp/streams", &[], &[]).await;
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
