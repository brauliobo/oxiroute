use std::{net::SocketAddr, path::PathBuf, str::FromStr as _};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, Listener, ListenerBind, Management, Protocol, Stats,
};
use oxiroute_supervisor_master::FailurePhase;
use serde_json::json;

use super::*;

fn config() -> ConfigDraft {
    serde_json::from_value(json!({
        "version": 1,
        "certificates": [{
            "name": "downstream",
            "dns_names": ["proxy.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/tmp/oxiroute-master-test-chain.pem",
                "private_key_path": "/tmp/oxiroute-master-test-key.pem"
            }
        }],
        "tls_profiles": [{
            "name": "h3",
            "certificates": ["downstream"],
            "default_certificate": "downstream",
            "min_version": "1.3",
            "alpn": ["h3"]
        }],
        "listeners": [],
        "http_services": [
            {
                "name": "web",
                "routes": [{
                    "path": {"kind": "segment_prefix", "value": "/"},
                    "action": {"type": "fixed_response", "status": 200}
                }]
            },
            {
                "name": "changed",
                "routes": [{
                    "path": {"kind": "segment_prefix", "value": "/"},
                    "action": {"type": "fixed_response", "status": 200}
                }]
            }
        ],
        "forward_proxy_services": [{
            "name": "forward",
            "enabled_versions": ["h1", "h2", "h3"],
            "tls_required": false
        }],
        "rtmp_services": [{
            "name": "live",
            "applications": [{"name": "broadcast", "live": true}]
        }],
        "upstream_pools": [{
            "name": "origin",
            "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]
        }],
        "l4_services": [{"name": "relay", "upstream_pool": "origin", "udp": {}}]
    }))
    .expect("supervised test config")
}

fn listener(name: &str, protocol: Protocol, tls_profile: Option<&str>) -> Listener {
    let service = match protocol {
        Protocol::Http | Protocol::Http3 => "web",
        Protocol::Rtmp => "live",
        Protocol::Tcp | Protocol::Udp => "relay",
        Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3 => "forward",
    };
    let bind = if matches!(
        protocol,
        Protocol::Udp | Protocol::Http3 | Protocol::ForwardHttp3
    ) {
        ListenerBind::Udp {
            address: SocketAddr::from(([127, 0, 0, 1], 8080)),
        }
    } else {
        ListenerBind::Socket {
            address: SocketAddr::from(([127, 0, 0, 1], 8080)),
        }
    };
    Listener {
        name: name.into(),
        bind,
        protocol,
        service: Some(service.into()),
        tls_profile: tls_profile.map(str::to_owned),
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    }
}

fn validated(config: &ConfigDraft) -> ValidatedConfig {
    config.clone().validate().expect("valid supervised config")
}

#[test]
fn eligibility_accepts_stream_listener_configuration() {
    let mut config = config();
    config
        .listeners
        .push(listener("http", Protocol::Http, None));
    assert_eq!(
        supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
        Ok(())
    );

    config.listeners[0] = listener("rtmp", Protocol::Rtmp, None);
    assert_eq!(
        supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
        Ok(())
    );

    config.listeners[0] = listener("forward-h3", Protocol::ForwardHttp3, Some("h3"));
    assert_eq!(
        supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
        Ok(())
    );

    config.listeners[0] = listener("udp", Protocol::Udp, None);
    assert_eq!(
        supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
        Ok(())
    );
}

#[test]
fn eligibility_uses_the_worker_descriptor_limit() {
    let mut config = config();
    config.listeners = (0..=oxiroute_supervision_unix::MAX_DESCRIPTOR_COUNT)
        .map(|index| {
            let mut listener = listener(&format!("listener-{index}"), Protocol::Http, None);
            listener.bind = ListenerBind::Socket {
                address: SocketAddr::from((
                    [127, 0, 0, 1],
                    10_000 + u16::try_from(index).expect("bounded listener index"),
                )),
            };
            listener
        })
        .collect();
    assert!(matches!(
        supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
        Err(UnsupportedConfig {
            reason: "Stage 2 worker listener descriptor limit is 64",
        })
    ));
}

#[test]
fn eligibility_accepts_authenticated_management_on_the_supervised_runtime() {
    let mut config = config();
    config.management = Some(Management {
        bind: SocketAddr::from(([127, 0, 0, 1], 9900)),
        ui_dir: None,
    });
    assert_eq!(
        supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
        Ok(())
    );
}

fn config_with_exec_profile() -> ConfigDraft {
    let mut config = config();
    config.rtmp_services[0].exec_profiles = vec![
        serde_json::from_value(json!({
            "name": "publisher",
            "application": "broadcast",
            "executable": "/bin/true",
            "working_directory": "/tmp"
        }))
        .expect("exec profile"),
    ];
    config
}

#[test]
fn config_plan_requires_supervision_and_ready_containment_for_exec_profiles() {
    let config = validated(&config_with_exec_profile());

    assert_eq!(
        config_plan(&config, true, CgroupV2ProbeStatus::Ready),
        Ok(ConfigPlan::Supervised)
    );
    assert_eq!(
        config_plan(&config, false, CgroupV2ProbeStatus::Ready),
        Err(UnsupportedConfig {
            reason: "RTMP exec profiles require the supervised launcher"
        })
    );
    for (status, reason) in [
        (
            CgroupV2ProbeStatus::Unsupported,
            "RTMP exec profiles require Linux cgroup-v2 containment",
        ),
        (
            CgroupV2ProbeStatus::Unavailable,
            "RTMP exec profiles require an available cgroup-v2 hierarchy",
        ),
        (
            CgroupV2ProbeStatus::ReadOnly,
            "RTMP exec profiles require writable cgroup-v2 delegation",
        ),
        (
            CgroupV2ProbeStatus::NotDelegated,
            "RTMP exec profiles require delegated cgroup-v2 containment",
        ),
        (
            CgroupV2ProbeStatus::MissingControllers,
            "RTMP exec profiles require the configured cgroup-v2 controllers",
        ),
    ] {
        let error = config_plan(&config, true, status).unwrap_err();
        assert_eq!(error, UnsupportedConfig { reason });
        assert!(!error.to_string().contains("/sys/fs/cgroup"));
    }
}

#[test]
fn config_plan_preserves_no_exec_direct_and_supervised_behavior() {
    let config = validated(&config());

    assert_eq!(
        config_plan(&config, false, CgroupV2ProbeStatus::Unavailable),
        Ok(ConfigPlan::Direct)
    );
    assert_eq!(
        config_plan(&config, true, CgroupV2ProbeStatus::Unavailable),
        Ok(ConfigPlan::Supervised)
    );
}

#[test]
fn worker_identity_preserves_token_and_fixed_generation() {
    let token = InstanceToken([0xab; 16]);
    let (instance_id, identity) = worker_identity(token).expect("worker identity");
    assert_eq!(instance_id.as_str(), MASTER_INSTANCE_ID);
    assert_eq!(identity.instance, token);
    assert_eq!(identity.generation, INITIAL_GENERATION);
    assert_eq!(identity.protocol, CONTROL_PROTOCOL_VERSION);
}

#[test]
fn replacement_identity_is_unique_and_monotonic() {
    let (_, first) = replacement_worker_identity(InstanceToken([0; 16]), GenerationId(2))
        .expect("first identity");
    let (second_id, second) = replacement_worker_identity(InstanceToken([1; 16]), GenerationId(3))
        .expect("second identity");

    assert_eq!(first.generation, GenerationId(2));
    assert_eq!(second.generation, GenerationId(3));
    assert_eq!(second_id.as_str(), "oxiroute-stage-3-3");
    assert_ne!(first.instance, second.instance);
}

fn launch_catalog() -> (MasterRunner, GenerationCatalog) {
    let runner = MasterRunner::new_for_test(
        "/test-only/oxiroute-worker-launcher",
        std::env::current_exe().expect("test executable"),
    );
    let revision = EffectiveRevision::from_str(&"a".repeat(64)).expect("active revision");
    let (instance_id, identity) =
        worker_identity(InstanceToken([0x11; 16])).expect("active identity");
    let active = runner
        .build_launch_document(
            Path::new("/etc/oxiroute/oxiroute.kdl"),
            validated(&config()),
            revision,
            instance_id,
            identity,
        )
        .expect("active launch document");
    (runner, SupervisedGenerationCatalog::new(active))
}

fn add_candidate(
    runner: &MasterRunner,
    catalog: &mut GenerationCatalog,
    revision_digit: char,
) -> InstanceId {
    let generation = catalog.allocate_generation().expect("candidate generation");
    let token_byte = u8::try_from(generation.0).expect("test generation fits token byte");
    let (instance_id, identity) =
        replacement_worker_identity(InstanceToken([token_byte; 16]), generation)
            .expect("candidate identity");
    let revision = EffectiveRevision::from_str(&revision_digit.to_string().repeat(64))
        .expect("candidate revision");
    let candidate = runner
        .build_launch_document(
            Path::new("/etc/oxiroute/oxiroute.kdl"),
            validated(&config()),
            revision,
            instance_id.clone(),
            identity,
        )
        .expect("candidate launch document");
    catalog
        .begin_candidate(candidate)
        .expect("catalog candidate");
    instance_id
}

#[test]
fn worker_input_is_derived_from_the_exact_catalog_candidate() {
    let (runner, mut catalog) = launch_catalog();
    let candidate_id = add_candidate(&runner, &mut catalog, 'b');
    let document = catalog.candidate().expect("candidate document");
    let input = worker_input(document);

    assert_eq!(input.instance_id, candidate_id);
    assert_eq!(input.identity, document.payload().identity);
    assert_eq!(input.identity.generation, document.generation_id());
    assert_eq!(
        format!("{:?}", input.command),
        format!("{:?}", document.payload().command)
    );
}

#[test]
fn exec_profile_launch_document_requires_cgroup_containment() {
    let runner = MasterRunner::new_for_test(
        "/test-only/oxiroute-worker-launcher",
        std::env::current_exe().expect("test executable"),
    );
    let revision = EffectiveRevision::from_str(&"a".repeat(64)).expect("revision");
    let (instance_id, identity) = worker_identity(InstanceToken([0x31; 16])).expect("identity");
    let document = runner
        .build_launch_document(
            Path::new("/etc/oxiroute/oxiroute.kdl"),
            validated(&config_with_exec_profile()),
            revision,
            instance_id,
            identity,
        )
        .expect("exec launch document");

    assert!(
        format!("{:?}", document.payload().command).contains("require_cgroup_containment: true")
    );
}

#[test]
fn catalog_maps_commit_and_completion_in_master_event_order() {
    let (runner, mut catalog) = launch_catalog();
    let retired = catalog.active().instance_id().clone();
    let active = add_candidate(&runner, &mut catalog, 'b');

    let mut book = pending_book(8081);
    apply_catalog_events(
        &mut catalog,
        &[
            MasterEvent::ReplacementCommitted {
                active: active.clone(),
                retired: retired.clone(),
            },
            MasterEvent::ReplacementCompleted {
                active: active.clone(),
            },
        ],
        &mut book,
    )
    .expect("ordered replacement events");

    assert_eq!(catalog.active().instance_id(), &active);
    assert_eq!(catalog.previous().unwrap().instance_id(), &retired);
    assert!(catalog.candidate().is_none());
    assert!(book.pending.is_none());
    assert_eq!(book.active_manifest, manifest_on(8081));
}

fn manifest_on(port: u16) -> DescriptorManifest {
    DescriptorManifest::new(vec![oxiroute_supervision_unix::DescriptorSlot {
        id: oxiroute_supervision_unix::SlotId(0),
        role: oxiroute_supervision_unix::DescriptorRole::Traffic("web".into()),
        kind: oxiroute_supervision_unix::DescriptorKind::TcpListener,
        bind: Some(oxiroute_supervision_unix::BindIdentity::Tcp(
            SocketAddr::from(([127, 0, 0, 1], port)),
        )),
        mode: None,
    }])
    .expect("one-slot manifest")
}

/// A book serving port 8080 with a replacement to `port` waiting for its outcome.
fn pending_book(port: u16) -> ListenerBook {
    let mut book = ListenerBook::new(ListenerReservations::default(), manifest_on(8080));
    book.pending = Some((ListenerReservations::default(), manifest_on(port)));
    book
}

#[test]
fn catalog_quarantines_rollback_and_spawn_failure_candidates() {
    let (runner, mut rollback_catalog) = launch_catalog();
    let active = rollback_catalog.active().instance_id().clone();
    let rollback = add_candidate(&runner, &mut rollback_catalog, 'b');
    let mut rollback_book = pending_book(8081);
    apply_catalog_events(
        &mut rollback_catalog,
        &[
            MasterEvent::RollbackStarted {
                candidate: rollback.clone(),
                phase: FailurePhase::Activation,
            },
            MasterEvent::RollbackCompleted { active },
        ],
        &mut rollback_book,
    )
    .expect("rollback events");
    assert!(rollback_book.pending.is_none());
    assert_eq!(rollback_book.active_manifest, manifest_on(8080));
    assert_eq!(
        rollback_catalog.quarantined().unwrap().instance_id(),
        &rollback
    );
    assert!(rollback_catalog.candidate().is_none());

    let (runner, mut spawn_catalog) = launch_catalog();
    let failed = add_candidate(&runner, &mut spawn_catalog, 'c');
    let mut spawn_book = pending_book(8081);
    apply_catalog_events(
        &mut spawn_catalog,
        &[MasterEvent::SpawnFailed {
            instance_id: failed.clone(),
        }],
        &mut spawn_book,
    )
    .expect("spawn failure event");
    assert!(spawn_book.pending.is_none());
    assert_eq!(spawn_book.active_manifest, manifest_on(8080));
    assert_eq!(spawn_catalog.quarantined().unwrap().instance_id(), &failed);
    assert!(spawn_catalog.candidate().is_none());
}

#[test]
fn catalog_retains_and_deduplicates_restart_required_revision() {
    let (runner, mut catalog) = launch_catalog();
    let generation = catalog.allocate_generation().expect("restart generation");
    let (restart_id, identity) = replacement_worker_identity(InstanceToken([0x22; 16]), generation)
        .expect("restart identity");
    let restart = runner
        .build_launch_document(
            Path::new("/etc/oxiroute/oxiroute.kdl"),
            validated(&config()),
            EffectiveRevision::from_str(&"d".repeat(64)).expect("restart revision"),
            restart_id.clone(),
            identity,
        )
        .expect("restart launch document");
    let revision = restart.revision().clone();
    catalog
        .record_restart_required(restart)
        .expect("restart-required document");

    assert_eq!(
        catalog.restart_required().unwrap().instance_id(),
        &restart_id
    );
    assert!(has_restart_required_revision(&catalog, &revision));
    assert!(!has_restart_required_revision(
        &catalog,
        &EffectiveRevision::from_str(&"e".repeat(64)).expect("other revision")
    ));
    assert_eq!(catalog.allocate_generation(), Ok(GenerationId(3)));
}

#[test]
fn a_repeated_rejection_is_reported_once_until_the_file_is_accepted() {
    let file = AuthoredRevision::from_str(&"a".repeat(64)).expect("revision");
    let other = AuthoredRevision::from_str(&"b".repeat(64)).expect("revision");
    let mut reports = ConfigReports::default();
    let refused =
        |file: &AuthoredRevision, code| refusal(file, code, "refused", String::from("why"));

    assert!(reports.record(&refused(&file, "E_LISTENER_BIND")));
    assert!(!reports.record(&refused(&file, "E_LISTENER_BIND")));
    assert!(reports.record(&refused(&other, "E_LISTENER_BIND")));
    assert!(reports.record(&refused(&other, "E_UNSUPPORTED_CONFIG")));
    assert!(reports.forget());
    assert!(!reports.forget());
    assert!(reports.record(&refused(&other, "E_UNSUPPORTED_CONFIG")));
}

#[test]
fn reload_drain_window_defaults_above_the_longest_stream_and_is_bounded() {
    use std::ffi::OsStr;

    assert_eq!(
        reload_drain_window(None).expect("default"),
        Duration::from_hours(3)
    );
    assert!(reload_drain_window(None).expect("default") > Duration::from_hours(2));
    assert_eq!(
        reload_drain_window(Some(OsStr::new(" 90 "))).expect("seconds"),
        Duration::from_secs(90)
    );
    assert_eq!(
        reload_drain_window(Some(OsStr::new("86400"))).expect("one day"),
        Duration::from_hours(24)
    );
    for invalid in ["0", "86401", "-1", "1h", "", "1.5"] {
        let error = reload_drain_window(Some(OsStr::new(invalid))).expect_err(invalid);
        assert!(error.to_string().contains(RELOAD_DRAIN_ENV), "{invalid}");
    }
}

#[test]
fn the_retiring_generation_sets_its_own_drain_window() {
    use std::ffi::OsStr;

    let plain = validated(&ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        ..config()
    });
    assert_eq!(retirement_window(&plain, None), Duration::from_hours(3));
    assert_eq!(
        retirement_window(&plain, Some(OsStr::new("90"))),
        Duration::from_secs(90)
    );

    let mut explicit = config();
    explicit.retirement = Some(oxiroute_config::Retirement {
        drain_deadline_ms: Some(45_000),
        max_retired_generations: 4,
    });
    let explicit = validated(&explicit);
    assert_eq!(
        retirement_window(&explicit, Some(OsStr::new("90"))),
        Duration::from_secs(45),
        "an explicit key wins over the environment"
    );
}

#[test]
fn master_config_reads_the_drain_window_from_the_environment() {
    let config = master_config_with_environment(|key: &str| {
        (key == RELOAD_DRAIN_ENV).then(|| OsString::from("120"))
    })
    .expect("configured drain");
    assert_eq!(config.drain_timeout(), Duration::from_mins(2));
    assert_eq!(config.quiesce_timeout(), Duration::from_secs(10));
    assert!(master_config_with_environment(|_: &str| Some(OsString::from("x"))).is_err());
}

#[test]
fn replacement_requires_the_same_listener_manifest() {
    let mut active = config();
    active
        .listeners
        .push(listener("http", Protocol::Http, None));
    let mut candidate = active.clone();
    candidate.listeners[0].service = Some("changed".into());
    assert!(ListenerReservations::same_supervised_listener_topology(
        &validated(&active),
        &validated(&candidate),
    ));

    candidate.listeners[0].name = "renamed".into();
    assert!(!ListenerReservations::same_supervised_listener_topology(
        &validated(&active),
        &validated(&candidate),
    ));

    candidate = active.clone();
    candidate.listeners[0].bind = ListenerBind::Socket {
        address: SocketAddr::from(([127, 0, 0, 1], 8081)),
    };
    assert!(!ListenerReservations::same_supervised_listener_topology(
        &validated(&active),
        &validated(&candidate),
    ));

    active = config();
    active.management = Some(Management {
        bind: SocketAddr::from(([127, 0, 0, 1], 9900)),
        ui_dir: None,
    });
    candidate = active.clone();
    candidate.management = Some(Management {
        bind: SocketAddr::from(([127, 0, 0, 1], 9901)),
        ui_dir: None,
    });
    assert!(!ListenerReservations::same_supervised_listener_topology(
        &validated(&active),
        &validated(&candidate),
    ));

    candidate = active.clone();
    candidate.stats = Some(Stats {
        binds: vec![SocketAddr::from(([127, 0, 0, 1], 8404))],
        admin_token_file: None,
        pages: Vec::new(),
    });
    assert!(!ListenerReservations::same_supervised_listener_topology(
        &validated(&active),
        &validated(&candidate),
    ));
}

#[test]
fn production_runner_uses_the_fixed_launcher_and_absolute_executable() {
    let runner = MasterRunner::production().expect("production runner");
    assert_eq!(runner.launcher_path, Path::new(PRODUCTION_LAUNCHER));
    assert!(runner.executable_path.is_absolute());
}

#[test]
fn worker_command_contains_metadata_but_not_launcher_configuration() {
    let executable = std::env::current_exe().expect("test executable");
    let launcher = PathBuf::from("/test-only/oxiroute-worker-launcher");
    let runner = MasterRunner::new_for_test(launcher.clone(), executable.clone());
    let revision = EffectiveRevision::from_str(&"a".repeat(64)).expect("revision");
    let (_, identity) = worker_identity(InstanceToken([0xcd; 16])).expect("identity");
    let command = runner
        .build_worker_command(Path::new("/etc/oxiroute/oxiroute.kdl"), &revision, identity)
        .expect("worker command");
    let debug = format!("{command:?}");

    assert!(debug.contains(executable.to_string_lossy().as_ref()));
    assert!(debug.contains(super::super::MARKER));
    assert!(debug.contains(", \"1\","));
    assert!(debug.contains(&"cd".repeat(16)));
    assert!(debug.contains("/etc/oxiroute/oxiroute.kdl"));
    assert!(debug.contains(&"a".repeat(64)));
    assert!(!debug.contains(launcher.to_string_lossy().as_ref()));

    let command = runner
        .build_worker_command_with_environment(
            Path::new("/etc/oxiroute/oxiroute.kdl"),
            &revision,
            identity,
            |key| match key {
                "OXIROUTE_MANAGEMENT_TOKEN_FILE" => {
                    Some(OsString::from("/etc/oxiroute/management.token"))
                }
                "OXIROUTE_AUDIT_DIR" => Some(OsString::from("/var/lib/oxiroute/audit")),
                "OXIROUTE_AUDIT_MAX_RECORDS" => Some(OsString::from("10000")),
                "OXIROUTE_AUDIT_MAX_RECORD_BYTES" => Some(OsString::from("16384")),
                "OXIROUTE_AUDIT_MAX_FILE_BYTES" => Some(OsString::from("1048576")),
                "OXIROUTE_AUDIT_MAX_TOTAL_BYTES" => Some(OsString::from("8388608")),
                "OXIROUTE_AUDIT_MAX_ROTATED_FILES" => Some(OsString::from("7")),
                _ => None,
            },
        )
        .expect("worker environment");
    let debug = format!("{command:?}");
    for expected in [
        "/etc/oxiroute/management.token",
        "/var/lib/oxiroute/audit",
        "10000",
        "16384",
        "1048576",
        "8388608",
        "7",
    ] {
        assert!(
            debug.contains(expected),
            "worker command omitted {expected}"
        );
    }
}

#[test]
fn the_retired_worker_is_reported_with_its_age_deadline_and_connections() {
    let mut status = worker_status_for_test();
    status.generation.active_revision = Some(format!("{:064x}", 5));
    status.metrics = Some(oxiroute_supervisor_master::WorkerMetrics {
        active_connections: 7,
        ..Default::default()
    });

    let report = remote_retired(&status, Duration::from_secs(20), Duration::from_secs(90))
        .expect("a worker with an active revision");
    assert_eq!(report.revision, format!("{:064x}", 5));
    assert_eq!(report.age_seconds, 20);
    assert_eq!(report.deadline_remaining_seconds, 70);
    assert_eq!(report.connections, 7);

    let overdue = remote_retired(&status, Duration::from_secs(95), Duration::from_secs(90));
    assert_eq!(overdue.expect("report").deadline_remaining_seconds, 0);

    status.generation.active_revision = None;
    assert!(remote_retired(&status, Duration::ZERO, Duration::ZERO).is_none());
}

fn worker_status_for_test() -> WorkerStatus {
    use oxiroute_supervisor_master::{
        WorkerAdministrativeState, WorkerGenerationStatus, WorkerLifecycle,
    };

    WorkerStatus {
        sequence: 1,
        generation_id: GenerationId(2),
        lifecycle: WorkerLifecycle::Draining,
        administrative_state: WorkerAdministrativeState::Ready,
        accepting: false,
        runtime_started: true,
        runtime_failed: false,
        drained: false,
        generation: WorkerGenerationStatus {
            disk_revision: None,
            candidate_revision: None,
            active_revision: None,
            previous_revision: None,
            quarantined_revision: None,
            active_accepting: false,
            degraded: false,
            last_failure: None,
            prepares: 0,
            activations: 0,
            failures: 0,
            rollbacks: 0,
        },
        metrics: None,
        listeners: Vec::new(),
        degraded: false,
        degradation: None,
        event_cursor: 0,
        event_cursor_lost: false,
        events: Vec::new(),
    }
}

#[test]
fn reload_monitor_ignores_files_that_are_not_the_configuration() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("oxiroute.kdl");
    std::fs::write(&path, "version 1\n").unwrap();
    let mut monitor = ConfigReloadMonitor::start(&path, &[], Waker::new().unwrap()).unwrap();
    let start = Instant::now();
    assert!(
        monitor.next_trigger(start),
        "the first reconciliation is immediate"
    );

    std::fs::write(directory.path().join("scratch.out"), b"x").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let quiet = start + CONFIG_RELOAD_DEBOUNCE * 4;
    assert!(!monitor.next_trigger(quiet));

    std::fs::write(&path, "version 1\n\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !monitor.next_trigger(quiet),
        "the debounce window opens at the first event"
    );
    assert!(monitor.next_trigger(quiet + CONFIG_RELOAD_DEBOUNCE));
}

fn readable(waker: &Waker, timeout: Duration) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    let mut descriptors = [PollFd::from_borrowed_fd(waker.descriptor(), PollFlags::IN)];
    let timeout = Timespec::try_from(timeout).expect("timeout");
    poll(&mut descriptors, Some(&timeout)).expect("poll") > 0
}

#[test]
fn a_waker_wakes_once_until_drained() {
    let waker = Waker::new().expect("waker");
    assert!(!readable(&waker, Duration::ZERO));

    waker.wake();
    waker.wake();
    assert!(readable(&waker, Duration::ZERO));
    waker.drain();
    assert!(!readable(&waker, Duration::ZERO));
}

#[test]
fn a_quiet_monitor_waits_for_the_periodic_reconciliation() {
    let directory = tempfile::tempdir().expect("config directory");
    let path = directory.path().join("oxiroute.kdl");
    let mut monitor =
        ConfigReloadMonitor::start(&path, &[], Waker::new().expect("waker")).expect("monitor");
    let now = Instant::now();

    assert!(monitor.next_trigger(now), "the first check is immediate");

    assert_eq!(
        monitor.next_deadline(),
        now + CONFIG_RECONCILIATION_INTERVAL
    );
    assert!(!monitor.next_trigger(now + Duration::from_secs(29)));
    assert!(monitor.next_trigger(now + CONFIG_RECONCILIATION_INTERVAL));
}

#[test]
fn a_file_event_wakes_the_loop_and_moves_the_deadline_to_the_end_of_the_debounce() {
    let directory = tempfile::tempdir().expect("config directory");
    let path = directory.path().join("oxiroute.kdl");
    let waker = Waker::new().expect("waker");
    let mut monitor = ConfigReloadMonitor::start(&path, &[], Arc::clone(&waker)).expect("monitor");
    let now = Instant::now();
    assert!(monitor.next_trigger(now));
    assert!(!readable(&waker, Duration::ZERO));

    std::fs::write(&path, "x").expect("write config");

    assert!(
        readable(&waker, Duration::from_secs(10)),
        "the watcher wakes the loop"
    );
    // One write raises several file events; let them all arrive before the monitor reads them.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !monitor.next_trigger(now),
        "the burst is still being debounced"
    );
    assert_eq!(monitor.next_deadline(), now + CONFIG_RELOAD_DEBOUNCE);
    assert!(monitor.next_trigger(now + CONFIG_RELOAD_DEBOUNCE));
}

#[test]
fn retired_reports_are_due_a_second_after_the_last_one() {
    let mut reports = RetiredReports::default();
    assert_eq!(reports.next_due(), None);

    let sent = Instant::now();
    reports.last_sent = Some(sent);

    assert_eq!(reports.next_due(), Some(sent + Duration::from_secs(1)));
}
