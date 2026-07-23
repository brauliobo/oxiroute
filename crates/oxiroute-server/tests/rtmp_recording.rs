#[path = "support/config.rs"]
mod config_support;
#[path = "support/rtmp.rs"]
mod rtmp_support;

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::{
    Config, Listener, Protocol, RtmpApplication, RtmpRecorderStart, RtmpService,
};
use oxiroute_rtmp::{RecorderPhase, RtmpRegistry};
use oxiroute_server::{RtmpManagementApi, RuntimeMetrics, ServiceKind, runtime_plan};
use serde_json::Value;
use tempfile::TempDir;

use config_support::{empty_config, loopback_address, rtmp_recorder, socket_bind};
use rtmp_support::RtmpSessionClient;

#[test]
fn continuous_recording_finalizes_on_disconnect_and_is_fully_observable() {
    let root = TempDir::new().expect("recording root");
    let config = recording_config(root.path(), RtmpRecorderStart::Continuous);
    let plan = runtime_plan(&config).expect("continuous runtime plan");
    let registry = Arc::new(RtmpRegistry::new(plan.rtmp_capabilities));
    let runtime = rtmp_runtime(&plan, Arc::clone(&registry));
    let metrics = RuntimeMetrics::new();
    metrics.set_rtmp_recording_supported(plan.rtmp_recording_supported);
    register_active_listeners(&config, &plan, &metrics);
    let api = RtmpManagementApi::new(Arc::clone(&registry), metrics, Arc::clone(&plan.topology));
    let mut publisher = publisher(&runtime, "camera?token=private");

    publish_audio(&mut publisher, 10, 0x11, 1_721_657_969_100);
    wait_for_phase(&registry, |phase| {
        matches!(phase, RecorderPhase::Recording { .. })
    });
    wait_until(Duration::from_secs(2), || {
        registry
            .snapshot()
            .streams
            .first()
            .and_then(|stream| stream.recorders.first())
            .is_some_and(|recorder| recorder.bytes_written > 0)
    });
    let stream = registry.snapshot().streams[0].clone();
    let unsupported_manual_path = format!(
        "/api/v1/rtmp/streams/{}/recorders/{}/start",
        stream.id, stream.recorders[0].id
    );
    let unsupported = api.handle("POST", &unsupported_manual_path, 1_721_657_969_150);
    assert_eq!(unsupported.status, 501);
    let unsupported_json: Value =
        serde_json::from_slice(&unsupported.body).expect("unsupported control JSON");
    assert_eq!(
        unsupported_json["error"]["code"],
        "rtmp_recording_unavailable"
    );
    let streams = api.handle("GET", "/api/v1/rtmp/streams", 1_721_657_969_200);
    let streams_json: Value = serde_json::from_slice(&streams.body).expect("streams JSON");
    let recorder = &streams_json["streams"][0]["recorders"][0];
    assert_eq!(streams_json["capabilities"]["manual_recording"], false);
    assert_eq!(streams_json["streams"][0]["recording_supported"], true);
    assert_eq!(recorder["phase"]["state"], "recording");
    assert!(
        recorder["bytes_written"]
            .as_str()
            .is_some_and(|bytes| bytes != "0")
    );
    assert_eq!(recorder["segments_started"], "1");
    assert_eq!(recorder["current_relative_name"], "camera.flv");

    let monitoring = api.handle("GET", "/api/v1/monitoring", 1_721_657_969_200);
    let monitoring_json: Value = serde_json::from_slice(&monitoring.body).expect("monitoring JSON");
    assert_eq!(monitoring_json["rtmp"]["recordingSupported"], true);
    assert_eq!(monitoring_json["rtmp"]["manualRecording"], false);
    assert_eq!(
        monitoring_json["rtmp"]["recorders"][0]["phase"],
        "recording"
    );
    assert_eq!(
        monitoring_json["rtmp"]["recorders"][0]["currentRelativeName"],
        "camera.flv"
    );

    let topology = api.handle("GET", "/api/v1/topology", 1_721_657_969_200);
    let topology_json: Value = serde_json::from_slice(&topology.body).expect("topology JSON");
    let listener = topology_json["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["kind"] == "rtmp_listener"))
        .expect("RTMP topology listener");
    assert_eq!(
        listener["attributes"]["applications"][0]["recording"]["recorderCount"],
        1
    );
    assert!(!String::from_utf8_lossy(&topology.body).contains(".flv"));
    let observable = format!(
        "{}{}{}",
        String::from_utf8_lossy(&streams.body),
        String::from_utf8_lossy(&monitoring.body),
        String::from_utf8_lossy(&topology.body),
    );
    assert!(!observable.contains(&root.path().display().to_string()));
    assert!(!observable.contains("token=private"));
    assert!(!observable.contains("-%Y-secret.flv"));

    publisher
        .server
        .close(1_721_657_969_300)
        .expect("publisher disconnect");
    assert!(registry.snapshot().streams.is_empty());
    wait_until(Duration::from_secs(2), || {
        root.path().join("camera.flv").is_file()
    });
    assert!(!root.path().join("camera?token=private.flv").exists());
    assert_eq!(stream.recorders.len(), 1);
}

#[test]
fn manual_api_start_and_stop_control_the_exact_runtime_recorder() {
    let root = TempDir::new().expect("recording root");
    let config = recording_config(root.path(), RtmpRecorderStart::Manual);
    let plan = runtime_plan(&config).expect("manual runtime plan");
    assert!(plan.rtmp_capabilities.manual_recording);
    let registry = Arc::new(RtmpRegistry::new(plan.rtmp_capabilities));
    let runtime = rtmp_runtime(&plan, Arc::clone(&registry));
    let metrics = RuntimeMetrics::new();
    metrics.set_rtmp_recording_supported(plan.rtmp_recording_supported);
    register_active_listeners(&config, &plan, &metrics);
    let api = RtmpManagementApi::new(Arc::clone(&registry), metrics, Arc::clone(&plan.topology));
    let mut publisher = publisher(&runtime, "manual-camera");
    let stream = registry.snapshot().streams[0].clone();
    let recorder_id = stream.recorders[0].id;

    let start_path = format!(
        "/api/v1/rtmp/streams/{}/recorders/{recorder_id}/start",
        stream.id
    );
    let started = api.handle("POST", &start_path, 1_721_657_969_100);
    assert_eq!(started.status, 200);
    let started_json: Value = serde_json::from_slice(&started.body).expect("start JSON");
    assert_eq!(started_json["phase"]["state"], "recording");

    publish_audio(&mut publisher, 10, 0x22, 1_721_657_969_200);
    wait_for_phase(&registry, |phase| {
        matches!(phase, RecorderPhase::Recording { .. })
    });
    let stop_path = format!(
        "/api/v1/rtmp/streams/{}/recorders/{recorder_id}/stop",
        stream.id
    );
    let stopped = api.handle("POST", &stop_path, 1_721_657_969_300);
    assert_eq!(stopped.status, 202);
    let stopped_json: Value = serde_json::from_slice(&stopped.body).expect("stop JSON");
    assert_eq!(stopped_json["phase"]["state"], "stopping");
    wait_for_phase(&registry, |phase| phase == RecorderPhase::Idle);
    wait_until(Duration::from_secs(2), || {
        root.path().join("manual-camera.flv").is_file()
    });
    publisher
        .server
        .close(1_721_657_969_400)
        .expect("publisher close");
}

fn recording_config(root_directory: &std::path::Path, start: RtmpRecorderStart) -> Config {
    Config {
        listeners: vec![Listener {
            name: "live".into(),
            bind: socket_bind(loopback_address(1935)),
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            max_connections: Some(16),
        }],
        rtmp_services: vec![RtmpService {
            name: "live".into(),
            applications: vec![RtmpApplication {
                name: "broadcast".into(),
                live: true,
                idle_streams: true,
                recorders: vec![rtmp_recorder("archive", start, root_directory)],
            }],
        }],
        ..empty_config()
    }
}

fn rtmp_runtime(
    plan: &oxiroute_server::RuntimePlan,
    registry: Arc<RtmpRegistry>,
) -> oxiroute_rtmp::RtmpServiceRuntime {
    let ServiceKind::Rtmp(service) = &plan.services[0].kind else {
        panic!("RTMP service plan");
    };
    service.runtime(registry).expect("RTMP service runtime")
}

fn register_active_listeners(
    config: &Config,
    plan: &oxiroute_server::RuntimePlan,
    metrics: &RuntimeMetrics,
) {
    for (listener, service) in config.listeners.iter().zip(&plan.services) {
        let listener_metrics = metrics
            .register_configured_listener(
                &listener.name,
                service.kind.protocol(),
                &listener.bind,
                listener.max_connections,
            )
            .expect("active listener metrics");
        listener_metrics.mark_listening();
    }
}

fn publisher(runtime: &oxiroute_rtmp::RtmpServiceRuntime, stream_name: &str) -> RtmpSessionClient {
    let mut publisher = RtmpSessionClient::connect(runtime, "broadcast");
    publisher.publish(stream_name, 1_721_657_969_000);
    publisher
}

fn publish_audio(publisher: &mut RtmpSessionClient, timestamp: u32, marker: u8, at_unix_ms: u64) {
    publisher.publish_audio(timestamp, &[0xaf, 0x01, marker], at_unix_ms);
}

fn wait_for_phase(registry: &RtmpRegistry, predicate: impl Fn(RecorderPhase) -> bool) {
    wait_until(Duration::from_secs(2), || {
        registry
            .snapshot()
            .streams
            .first()
            .and_then(|stream| stream.recorders.first())
            .is_some_and(|recorder| predicate(recorder.phase))
    });
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timeout");
        thread::sleep(Duration::from_millis(5));
    }
}
