use std::sync::mpsc;

use oxiroute_config::{ConfigDraft, Protocol, Stats};
use oxiroute_config_source::{ConfigFormat, render_config};
use serde_json::json;

use super::{status::*, *};
use crate::test_ports::unused_tcp_address;

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
    let listener_address = unused_tcp_address();
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
        token_file: None,
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
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
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

#[test]
fn a_retired_worker_notice_lists_and_then_clears_the_draining_worker() {
    let manager = GenerationManager::new();
    let retired = WorkerNotice::Retired {
        generations: vec![oxiroute_server::RemoteRetired {
            revision: format!("{:064x}", 9),
            age_seconds: 4,
            deadline_remaining_seconds: 100,
            connections: 2,
        }],
    };
    apply_notice(&manager, &serde_json::to_vec(&retired).unwrap()).expect("retired notice");
    let listed = manager.status().retired_generations;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].in_flight, 2);

    let cleared = WorkerNotice::Retired {
        generations: Vec::new(),
    };
    apply_notice(&manager, &serde_json::to_vec(&cleared).unwrap()).expect("empty notice");
    assert!(manager.status().retired_generations.is_empty());
}

#[test]
fn a_configuration_notice_still_clears_the_reported_rejection() {
    let manager = GenerationManager::new();
    let accepted = WorkerNotice::Config { rejection: None };
    apply_notice(&manager, &serde_json::to_vec(&accepted).unwrap()).expect("config notice");
    assert!(manager.status().last_rejection.is_none());
    assert!(
        apply_notice(&manager, b"null").is_err(),
        "the old bare format is gone"
    );
}

fn observed_status() -> WorkerStatus {
    WorkerStatus {
        sequence: 7,
        generation_id: GenerationId(1),
        lifecycle: WorkerLifecycle::Active,
        administrative_state: WorkerAdministrativeState::Ready,
        accepting: true,
        runtime_started: true,
        runtime_failed: false,
        drained: false,
        generation: WorkerGenerationStatus {
            disk_revision: None,
            candidate_revision: None,
            active_revision: None,
            previous_revision: None,
            quarantined_revision: None,
            active_accepting: true,
            degraded: false,
            last_failure: None,
            prepares: 1,
            activations: 1,
            failures: 0,
            rollbacks: 0,
        },
        metrics: Some(WorkerMetrics::default()),
        listeners: Vec::new(),
        degraded: false,
        degradation: None,
        event_cursor: 3,
        event_cursor_lost: false,
        events: Vec::new(),
    }
}

#[test]
fn a_status_that_only_advances_its_sequence_is_not_a_new_observation() {
    let last = observed_status();
    let mut next = observed_status();
    next.sequence = 8;

    assert!(is_new_observation(None, &next));
    assert!(!is_new_observation(Some(&last), &next));
}

#[test]
fn a_changed_counter_or_event_is_a_new_observation() {
    let last = observed_status();
    let mut traffic = observed_status();
    traffic.metrics = Some(WorkerMetrics {
        accepted_connections: 1,
        ..WorkerMetrics::default()
    });
    let mut event = observed_status();
    event.event_cursor = 4;

    assert!(is_new_observation(Some(&last), &traffic));
    assert!(is_new_observation(Some(&last), &event));
}
