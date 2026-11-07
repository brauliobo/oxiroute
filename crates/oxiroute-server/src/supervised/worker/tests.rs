use std::sync::mpsc;

use oxiroute_config::{ConfigDraft, Protocol, Stats};
use oxiroute_config_source::{ConfigFormat, render_config};
use serde_json::json;

use super::*;

#[test]
fn worker_listener_statuses_are_exact_real_snapshots_without_synthesis() {
    let snapshots = vec![oxiroute_server::ListenerSnapshot {
        administrative_state: AdministrativeState::Ready,
        name: "@management".into(),
        protocol: "http".into(),
        bind: "socket:127.0.0.1:9900".into(),
        max_connections: None,
        state: ListenerRuntimeState::Listening,
        accepted_connections: 3,
        rejected_connections: 1,
        active_connections: 2,
        bytes_received: 11,
        bytes_sent: 13,
        http_operations: None,
        tcp_relays: None,
        proxy_protocol: None,
        cache: None,
    }];

    let statuses = listener_statuses(&snapshots);

    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].name, "@management");
    assert_eq!(statuses[0].state, WorkerListenerState::Listening);
    assert_eq!(statuses[0].accepted_connections, 3);
    assert_eq!(statuses[0].rejected_connections, 1);
    assert_eq!(statuses[0].active_connections, 2);
    assert_eq!(statuses[0].bytes_received, 11);
    assert_eq!(statuses[0].bytes_sent, 13);
}

#[test]
fn deferred_lifecycle_phases_are_supported() {
    let manager = GenerationManager::new();
    let mut quiesced = None;

    assert!(matches!(
        quiesce_active(&manager, &mut quiesced),
        ControlOutcome::Rejected(REJECT_RUNTIME)
    ));
    assert!(!start_drain(&manager));
    assert!(matches!(
        reactivate_active(&mut quiesced),
        ControlOutcome::Rejected(REJECT_RUNTIME)
    ));
}

#[test]
fn prepublication_runtime_failure_retires_recording_authority_before_quarantine() {
    assert_prepublication_recording_cleanup("runtime_failure");
}

#[test]
fn prepublication_control_error_retires_recording_authority_before_quarantine() {
    assert_prepublication_recording_cleanup("control_error");
}

#[test]
fn prepublication_shutdown_retires_recording_authority_before_quarantine() {
    assert_prepublication_recording_cleanup("shutdown");
}

fn assert_prepublication_recording_cleanup(branch: &str) {
    let directory = tempfile::tempdir().expect("supervised recording directory");
    let root = directory.path().join("recordings");
    std::fs::create_dir(&root).expect("recording root");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("RTMP listener probe");
    let listener_address = listener.local_addr().expect("RTMP listener address");
    drop(listener);
    let config: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [{
            "name": "ingest",
            "bind": {"type": "socket", "address": listener_address},
            "protocol": "rtmp",
            "service": "live"
        }],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "live",
                "live": true,
                "recorders": [{"name": "archive", "root_directory": root}]
            }]
        }]
    }))
    .expect("recording config");
    let path = directory.path().join("oxiroute.kdl");
    std::fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid recording config"),
        )
        .expect("render recording config"),
    )
    .expect("write recording config");
    let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("recording config rejected")
    };
    let manager = GenerationManager::new();
    let candidate = manager.prepare(*document).expect("candidate");
    let mut startup = Some(
        manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation"),
    );
    let generation = startup
        .as_mut()
        .expect("startup")
        .claim_runtime_start()
        .expect("runtime start");
    let detached_runtime = generation.rtmp_service("live").expect("RTMP runtime");
    let (release, released) = mpsc::sync_channel(0);
    let (finished, completion) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        released.recv().expect("release detached worker");
        drop(detached_runtime.session());
        finished.send(()).expect("detached completion receiver");
    });
    let (shutdown, _receiver) = tokio::sync::watch::channel(false);
    let process = GenerationProcess {
        retirement: crate::CandidateRtmpRetirementHandle::capture(&generation),
        generation,
        shutdown,
        thread,
    };
    let started = Instant::now();

    assert!(finish_worker_generation_process(
        &manager,
        process,
        true,
        started + Duration::from_millis(75),
    ));
    drop(startup.take());

    assert!(started.elapsed() < Duration::from_millis(250), "{branch}");
    assert_eq!(manager.status().failures, 1, "{branch}");
    let validated = config.clone().validate().expect("retry config");
    let service = oxiroute_server::service_specs(&validated)
        .expect("retry service plans")
        .into_iter()
        .find_map(|service| match service.kind {
            oxiroute_server::ServiceKind::Rtmp(service) => Some(service.value_plan()),
            _ => None,
        })
        .expect("RTMP retry plan");
    drop(
        oxiroute_rtmp::PreparedRtmpRuntimeSet::prepare(
            [service],
            &oxiroute_rtmp::RtmpPrepareContext::new(oxiroute_rtmp::RtmpPrepareMode::Activation, []),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_or_else(|error| panic!("{branch} recording retry failed: {error}")),
    );
    release.send(()).expect("release detached worker");
    completion
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_else(|error| panic!("{branch} detached exit failed: {error}"));
}

#[test]
fn stage_two_configuration_accepts_stream_listeners_and_statistics() {
    let mut config: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "http_services": [{
            "name": "http",
            "routes": [{
                "path": {"kind": "segment_prefix", "value": "/"},
                "action": {"type": "fixed_response", "status": 200}
            }]
        }],
        "upstream_pools": [{
            "name": "origin",
            "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]
        }],
        "l4_services": [{"name": "relay", "upstream_pool": "origin", "udp": {}}]
    }))
    .expect("stage two test config");
    assert!(validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok());

    config.management = Some(oxiroute_config::Management {
        bind: "127.0.0.1:9900".parse().expect("management address"),
        ui_dir: None,
    });
    assert!(validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok());
    config.management = None;

    config.listeners.push(oxiroute_config::Listener {
        name: "http".into(),
        bind: oxiroute_config::ListenerBind::Socket {
            address: "127.0.0.1:8080".parse().expect("address"),
        },
        protocol: Protocol::Http,
        service: Some("http".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    assert!(validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok());

    config.stats = Some(Stats {
        binds: vec!["127.0.0.1:8404".parse().expect("statistics address")],
        admin_token_file: None,
        pages: Vec::new(),
    });
    assert!(validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok());

    config.listeners[0].bind = oxiroute_config::ListenerBind::Udp {
        address: "127.0.0.1:8080".parse().expect("UDP address"),
    };
    config.listeners[0].protocol = Protocol::Udp;
    config.listeners[0].service = Some("relay".into());
    assert!(validate_stage_one_config(&config.validate().expect("valid config")).is_ok());
}
