#[allow(clippy::wildcard_imports)]
use super::*;
use crate::test_ports::unused_tcp_address;

#[tokio::test]
async fn generation_runtime_marker_publishes_readiness_after_start_claim() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("oxiroute.kdl");
    let config = ConfigDraft {
        listener_workers: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
        audit: None,
    };
    fs::write(
        &path,
        oxiroute_config_source::render_config(
            oxiroute_config_source::ConfigFormat::Kdl,
            &config.validate().expect("valid config"),
        )
        .expect("render"),
    )
    .expect("config");
    let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("load");
    };
    let manager = GenerationManager::new();
    let candidate = manager.prepare(*document).expect("candidate");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("startup reservation");
    let generation = startup.claim_runtime_start().expect("runtime start claim");
    let marker = GenerationRuntimeMarker {
        generation: Arc::clone(&generation),
    };
    let (shutdown_tx, shutdown) = watch::channel(false);
    let (ready_tx, mut ready) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut marker = marker;
        marker
            .start_service(None, shutdown, 1, ServiceReadyNotifier::new(ready_tx))
            .await;
    });

    ready.changed().await.expect("generation readiness");
    assert!(*ready.borrow());
    assert!(generation.runtime_started());

    shutdown_tx.send(true).expect("generation shutdown");
    task.await.expect("generation marker task");
    drop(startup);
}

fn acme_supervisor_probe(retained: Arc<()>) -> Arc<AcmeSupervisorProbe> {
    Arc::new(AcmeSupervisorProbe {
        iterations: AtomicU64::new(0),
        stops: AtomicU64::new(0),
        joins: AtomicU64::new(0),
        retained: Mutex::new(Some(retained)),
        block_worker: false,
        worker_cleanups: AtomicU64::new(0),
    })
}

fn wait_for_acme_iteration(probe: &AcmeSupervisorProbe) {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while probe.iterations.load(Ordering::Acquire) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "ACME worker did not start"
        );
        thread::yield_now();
    }
}

#[test]
fn acme_supervisor_shutdown_is_idempotent_and_releases_worker_authority() {
    let retained = Arc::new(());
    let weak = Arc::downgrade(&retained);
    let probe = acme_supervisor_probe(retained);
    let mut supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
        .expect("ACME supervisor");
    wait_for_acme_iteration(&probe);

    supervisor.shutdown();
    supervisor.shutdown();
    drop(supervisor);

    assert_eq!(probe.stops.load(Ordering::Acquire), 1);
    assert_eq!(probe.joins.load(Ordering::Acquire), 1);
    assert!(weak.upgrade().is_none());
}

#[test]
fn acme_supervisor_cancels_controlled_wait_and_cleans_worker_once() {
    let retained = Arc::new(());
    let weak = Arc::downgrade(&retained);
    let probe = Arc::new(AcmeSupervisorProbe {
        iterations: AtomicU64::new(0),
        stops: AtomicU64::new(0),
        joins: AtomicU64::new(0),
        retained: Mutex::new(Some(retained)),
        block_worker: true,
        worker_cleanups: AtomicU64::new(0),
    });
    let mut supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
        .expect("ACME supervisor");
    wait_for_acme_iteration(&probe);
    let started = std::time::Instant::now();

    supervisor.shutdown();
    supervisor.shutdown();

    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(probe.stops.load(Ordering::Acquire), 1);
    assert_eq!(probe.joins.load(Ordering::Acquire), 1);
    assert_eq!(probe.worker_cleanups.load(Ordering::Acquire), 1);
    assert!(weak.upgrade().is_none());
}

#[test]
fn disconnected_acme_stop_channel_terminates_without_spinning() {
    let retained = Arc::new(());
    let weak = Arc::downgrade(&retained);
    let probe = acme_supervisor_probe(retained);
    let mut supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
        .expect("ACME supervisor");
    wait_for_acme_iteration(&probe);

    drop(supervisor.stop.take());
    let thread = supervisor.thread.take().expect("ACME worker");
    thread.join().expect("disconnected worker exits");
    let iterations = probe.iterations.load(Ordering::Acquire);
    thread::sleep(Duration::from_millis(10));

    assert_eq!(probe.iterations.load(Ordering::Acquire), iterations);
    assert_eq!(iterations, 1);
    assert!(weak.upgrade().is_none());
}

#[test]
fn later_runtime_join_failures_drop_and_join_acme_supervisor_once() {
    for (http3, udp, expected) in [
        (
            vec![InjectedRuntimeJoin {
                result: Err(io::Error::other("injected H3 join failure")),
            }],
            vec![InjectedRuntimeJoin { result: Ok(()) }],
            "injected H3 join failure",
        ),
        (
            vec![InjectedRuntimeJoin { result: Ok(()) }],
            vec![InjectedRuntimeJoin {
                result: Err(io::Error::other("injected UDP join failure")),
            }],
            "injected UDP join failure",
        ),
    ] {
        let retained = Arc::new(());
        let weak = Arc::downgrade(&retained);
        let probe = acme_supervisor_probe(retained);
        let supervisor = AcmeManagedSupervisor::start_internal(Vec::new(), Some(probe.clone()))
            .expect("ACME supervisor");
        wait_for_acme_iteration(&probe);

        let error =
            join_generation_runtimes(supervisor, http3, udp).expect_err("injected join failure");

        assert_eq!(error.to_string(), expected);
        assert_eq!(probe.stops.load(Ordering::Acquire), 1);
        assert_eq!(probe.joins.load(Ordering::Acquire), 1);
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn active_runtime_death_fails_the_generation_supervisor() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("oxiroute.kdl");
    let recording_root = directory.path().join("recordings");
    fs::create_dir(&recording_root).expect("recording root");
    let listener_address = unused_tcp_address();
    let config = recorder_runtime_config(listener_address, &recording_root);
    let (coordinator, manager, generation) = activate_test_generation(&path, &config);
    let ownership = fs::File::open(&recording_root).expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
    publish_stalled_recorder(&generation, "live");
    let (shutdown, _receiver) = tokio::sync::watch::channel(false);
    let thread = thread::spawn(|| {});
    while !thread.is_finished() {
        thread::yield_now();
    }
    let process = GenerationProcess {
        retirement: CandidateRtmpRetirementHandle::capture(&generation),
        generation,
        shutdown,
        thread,
    };
    let stop = AtomicBool::new(false);
    let process_shutdown = Arc::new(AtomicBool::new(false));
    let started = std::time::Instant::now();

    assert!(!supervise_generations(
        &manager,
        &coordinator,
        process,
        &stop,
        &process_shutdown,
    ));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(500),
        "runtime failure bypassed bounded recorder shutdown: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(5));
    assert!(process_shutdown.load(Ordering::Acquire));
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
}

#[test]
fn post_activation_watcher_failure_runs_bounded_recorder_cleanup() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("oxiroute.kdl");
    let recording_root = directory.path().join("recordings");
    fs::create_dir(&recording_root).expect("recording root");
    let listener_address = unused_tcp_address();
    let config = recorder_runtime_config(listener_address, &recording_root);
    let (coordinator, manager, generation) = activate_test_generation(&path, &config);
    let ownership = fs::File::open(&recording_root).expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
    let (shutdown, _receiver) = tokio::sync::watch::channel(false);
    let thread = thread::spawn(|| {});
    while !thread.is_finished() {
        thread::yield_now();
    }
    let initial = Arc::new(Mutex::new(Some(GenerationProcess {
        retirement: CandidateRtmpRetirementHandle::capture(&generation),
        generation: Arc::clone(&generation),
        shutdown,
        thread,
    })));
    let started = std::time::Instant::now();

    let result =
        start_config_watcher_or_shutdown(coordinator, &manager, &initial, move |_, _, _, _| {
            publish_stalled_recorder(&generation, "live");
            Err(notify::Error::generic("injected watcher failure"))
        });
    let elapsed = started.elapsed();

    assert!(result.is_err());
    assert!(initial.lock().expect("initial process slot").is_none());
    assert!(
        elapsed >= Duration::from_millis(500),
        "watcher failure bypassed recorder cleanup: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(5));
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
}

#[test]
fn generation_join_detaches_at_the_absolute_process_deadline() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("oxiroute.kdl");
    let config = ConfigDraft {
        listener_workers: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
        audit: None,
    };
    let (_, manager, generation) = activate_test_generation(&path, &config);
    let (release, released) = mpsc::sync_channel(0);
    let (finished, completion) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        released.recv().expect("release stuck generation");
        finished.send(()).expect("generation completion receiver");
    });
    let (shutdown, _receiver) = tokio::sync::watch::channel(false);
    let process = GenerationProcess {
        retirement: CandidateRtmpRetirementHandle::capture(&generation),
        generation,
        shutdown,
        thread,
    };
    let started = std::time::Instant::now();

    let outcome = process.join_until(started + Duration::from_millis(75));
    let elapsed = started.elapsed();

    assert_eq!(outcome, GenerationJoinOutcome::Detached);
    assert!(elapsed >= Duration::from_millis(50));
    assert!(elapsed < Duration::from_millis(250));
    release.send(()).expect("release generation thread");
    completion
        .recv_timeout(Duration::from_secs(1))
        .expect("detached generation completion");

    let thread = thread::spawn(|| panic!("injected completed generation panic"));
    while !thread.is_finished() {
        thread::yield_now();
    }
    let (shutdown, _receiver) = tokio::sync::watch::channel(false);
    let generation = manager.active().expect("active generation");
    let completed = GenerationProcess {
        retirement: CandidateRtmpRetirementHandle::capture(&generation),
        generation,
        shutdown,
        thread,
    };
    assert_eq!(
        completed.join_until(std::time::Instant::now()),
        GenerationJoinOutcome::Panicked
    );
}

#[test]
fn candidate_failure_cleanup_detaches_at_its_absolute_deadline() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("oxiroute.kdl");
    let recording_root = directory.path().join("recordings");
    fs::create_dir(&recording_root).expect("recording root");
    let listener_address = unused_tcp_address();
    let config = recorder_runtime_config(listener_address, &recording_root);
    fs::write(
        &path,
        oxiroute_config_source::render_config(
            oxiroute_config_source::ConfigFormat::Kdl,
            &config.clone().validate().expect("valid config"),
        )
        .expect("render"),
    )
    .expect("config");
    let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("load")
    };
    let manager = GenerationManager::new();
    let candidate = manager.prepare(*document).expect("candidate");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("startup reservation");
    let generation = startup.claim_runtime_start().expect("runtime start claim");
    let detached_runtime = generation.rtmp_service("live").expect("RTMP runtime");
    let (release, released) = mpsc::sync_channel(0);
    let (finished, completion) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        released.recv().expect("release candidate process");
        assert!(detached_runtime.session().receive(&[], 0).is_ok());
        finished.send(()).expect("candidate completion receiver");
    });
    let (shutdown, _receiver) = tokio::sync::watch::channel(false);
    let process = GenerationProcess {
        retirement: CandidateRtmpRetirementHandle::capture(&generation),
        generation,
        shutdown,
        thread,
    };
    let started = std::time::Instant::now();

    assert!(finish_candidate_generation_process(
        &manager,
        process,
        started + Duration::from_millis(75),
    ));
    let elapsed = started.elapsed();

    assert!(elapsed >= Duration::from_millis(50));
    assert!(elapsed < Duration::from_millis(250));
    let validated = config.clone().validate().expect("retry config");
    let service = oxiroute_server::service_specs(&validated)
        .expect("retry service plans")
        .into_iter()
        .find_map(|service| match service.kind {
            ServiceKind::Rtmp(service) if service.service_id() == "live" => {
                Some(service.value_plan())
            }
            _ => None,
        })
        .expect("live retry plan");
    drop(
        oxiroute_rtmp::PreparedRtmpRuntimeSet::prepare(
            [service],
            &oxiroute_rtmp::RtmpPrepareContext::new(oxiroute_rtmp::RtmpPrepareMode::Activation, []),
            std::time::Instant::now() + Duration::from_secs(1),
        )
        .expect("detached candidate released recording root"),
    );
    release.send(()).expect("release detached candidate");
    completion
        .recv_timeout(Duration::from_secs(1))
        .expect("detached candidate exit");
}

#[test]
#[allow(clippy::too_many_lines)]
fn candidate_cleanup_visits_later_services_after_the_first_shutdown_times_out() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("oxiroute.kdl");
    let first_root = directory.path().join("first-recordings");
    let second_root = directory.path().join("second-recordings");
    fs::create_dir(&first_root).expect("first recording root");
    fs::create_dir(&second_root).expect("second recording root");
    let first_address = unused_tcp_address();
    let second_address = unused_tcp_address();
    let mut config = recorder_runtime_config(first_address, &first_root);
    let mut second_service = config.rtmp_services[0].clone();
    second_service.name = "second".into();
    second_service.applications[0].recorders[0].root_directory = second_root.clone();
    config.rtmp_services.push(second_service);
    let mut second_listener = config.listeners[0].clone();
    second_listener.name = "second".into();
    second_listener.bind = ListenerBind::Socket {
        address: second_address,
    };
    second_listener.service = Some("second".into());
    config.listeners.push(second_listener);
    fs::write(
        &path,
        oxiroute_config_source::render_config(
            oxiroute_config_source::ConfigFormat::Kdl,
            &config.clone().validate().expect("valid config"),
        )
        .expect("render"),
    )
    .expect("config");
    let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("load")
    };
    let manager = GenerationManager::new();
    let candidate = manager.prepare(*document).expect("candidate");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("startup reservation");
    let generation = startup.claim_runtime_start().expect("runtime start claim");
    let ownership = fs::File::open(&first_root).expect("first root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall first recorder");
    publish_stalled_recorder(&generation, "live");
    let (release, released) = mpsc::sync_channel(0);
    let thread = thread::spawn(move || released.recv().expect("release candidate process"));
    let (shutdown, _receiver) = tokio::sync::watch::channel(false);
    let process = GenerationProcess {
        retirement: CandidateRtmpRetirementHandle::capture(&generation),
        generation,
        shutdown,
        thread,
    };
    let deadline = std::time::Instant::now() + Duration::from_millis(75);

    // The stalled recorder is only cancelled once the deadline passes, so whether its exit is seen
    // before this call returns is up to scheduling; the retry below observes the released roots.
    let _ = finish_candidate_generation_process(&manager, process, deadline);

    let retry_path = directory.path().join("retry.kdl");
    let mut retry_config = config.clone();
    retry_config.listeners[0].bind = ListenerBind::Socket {
        address: unused_tcp_address(),
    };
    retry_config.listeners[1].bind = ListenerBind::Socket {
        address: unused_tcp_address(),
    };
    fs::write(
        &retry_path,
        oxiroute_config_source::render_config(
            oxiroute_config_source::ConfigFormat::Kdl,
            &retry_config.clone().validate().expect("retry config"),
        )
        .expect("render retry config"),
    )
    .expect("write retry config");
    let retry_coordinator =
        CanonicalConfigCoordinator::new(&retry_path).expect("retry coordinator");
    flock(&ownership, FlockOperation::Unlock).expect("release first recording root");
    release.send(()).expect("release detached candidate");
    let ConfigLoadOutcome::Loaded(retry_document) = retry_coordinator.load() else {
        panic!("retry config after release")
    };
    let retry_manager = GenerationManager::new();
    assert!(
        retry_manager
            .prepare_with_deadline(
                *retry_document,
                std::time::Instant::now() + Duration::from_secs(1),
            )
            .is_ok()
    );
}
