#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn initial_duplicate_and_send_faults_fail_closed_without_synchronous_reap() {
    for fault in [
        FaultPoint::Duplicate(WorkerRole::Active),
        FaultPoint::Send(WorkerRole::Active, ControlPhase::AdoptListeners),
        FaultPoint::Send(WorkerRole::Active, ControlPhase::Activate),
    ] {
        let mut executor = FaultExecutor::default();
        executor.fail(fault);
        let (_temporary, listeners, _, _, _, _) = listeners();
        let mut factory = factory();
        let mut master = Master::launch_with_executor(
            config(),
            listeners,
            executor,
            &mut factory,
            worker("a", 1, "normal"),
            Instant::now(),
        )
        .unwrap();
        let started = Instant::now();
        let events = poll_until(&mut master, |master| master.state() == MasterState::Failed);
        assert!(started.elapsed() < TEST_TIMEOUT);
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::ActionFailed { action, .. }
                if *action == fault.action()
        )));
    }
}

#[test]
pub(super) fn candidate_duplicate_and_precommit_send_faults_compensate_to_active() {
    for fault in [
        FaultPoint::Duplicate(WorkerRole::Candidate),
        FaultPoint::Send(WorkerRole::Candidate, ControlPhase::AdoptListeners),
        FaultPoint::Send(WorkerRole::Active, ControlPhase::Quiesce),
        FaultPoint::Send(WorkerRole::Candidate, ControlPhase::Activate),
    ] {
        let mut harness = Harness::launch_with_executor("normal", FaultExecutor::default());
        harness.master.action_executor_mut().fail(fault);
        harness.replace("normal", 2);
        let events = harness.until(|master| {
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|instance| instance.as_str() == "a")
        });
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::ActionFailed { action, .. }
                if *action == fault.action()
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, MasterEvent::RollbackCompleted { .. }))
        );
        assert_served_by(&harness, 1);
        harness.shutdown();
    }
}

#[test]
pub(super) fn retired_send_faults_keep_candidate_and_track_old_until_reaped() {
    for fault in [
        FaultPoint::Send(WorkerRole::Retired, ControlPhase::Drain),
        FaultPoint::Send(WorkerRole::Retired, ControlPhase::Shutdown),
    ] {
        let mut harness = Harness::launch_with_executor("normal", FaultExecutor::default());
        harness.master.action_executor_mut().fail(fault);
        harness.replace("normal", 2);
        let mut saw_terminating = false;
        let events = harness.until(|master| {
            saw_terminating |= matches!(
                master.worker_state(WorkerRole::Retired),
                Some(WorkerState::Terminating { forced: true })
            );
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|instance| instance.as_str() == "b")
        });
        assert!(saw_terminating);
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::ActionFailed { action, .. }
                if *action == fault.action()
        )));
        assert_served_by(&harness, 2);
        harness.shutdown();
    }
}

#[test]
pub(super) fn reactivation_send_fault_fails_closed_and_shutdown_send_fault_is_forced_pending() {
    let mut rollback = Harness::launch_with_executor("normal", FaultExecutor::default());
    rollback.master.action_executor_mut().fail(FaultPoint::Send(
        WorkerRole::Active,
        ControlPhase::Reactivate,
    ));
    rollback.replace("reject-activate", 2);
    let events = rollback.until(|master| master.state() == MasterState::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ActionFailed {
            action: ActionKind::Send(ControlPhase::Reactivate),
            ..
        }
    )));

    let mut shutdown = Harness::launch_with_executor("normal", FaultExecutor::default());
    shutdown
        .master
        .action_executor_mut()
        .fail(FaultPoint::Send(WorkerRole::Active, ControlPhase::Shutdown));
    let started = Instant::now();
    assert_eq!(
        shutdown.master.shutdown(Instant::now()).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: true
        }
    );
    assert!(started.elapsed() < Duration::from_millis(100));
    shutdown.until(|master| master.state() == MasterState::Stopped);
}

#[test]
pub(super) fn candidate_adoption_preparation_faults_are_compensated_without_stranding() {
    for step in [
        PreparationStep::RequestId,
        PreparationStep::Deadline,
        PreparationStep::Encoding,
        PreparationStep::Allocation,
    ] {
        let mut harness = Harness::launch_with_executor("normal", FaultExecutor::default());
        harness
            .master
            .action_executor_mut()
            .fail(FaultPoint::Prepare(
                WorkerRole::Candidate,
                ControlPhase::AdoptListeners,
                step,
            ));
        harness.replace("normal", 2);
        let events = harness.until(|master| {
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|instance| instance.as_str() == "a")
        });
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::PreparationFailed {
                role: WorkerRole::Candidate,
                phase: ControlPhase::AdoptListeners,
                step: actual,
            } if *actual == step
        )));
        assert_eq!(harness.master.worker_state(WorkerRole::Candidate), None);
        assert_eq!(harness.master.next_deadline(), None);
        assert_served_by(&harness, 1);
        harness.shutdown();
    }
}

#[test]
pub(super) fn preparation_faults_after_lifecycle_mutation_compensate_or_fail_closed() {
    let mut quiesce = Harness::launch_with_executor("delay-reactivate", FaultExecutor::default());
    quiesce
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Active,
            ControlPhase::Quiesce,
            PreparationStep::Deadline,
        ));
    quiesce.replace("normal", 2);
    let mut events = quiesce.until(|master| master.state() == MasterState::RollingBack);
    assert!(quiesce.master.next_deadline().is_some());
    events.extend(quiesce.until(|master| master.state() == MasterState::Running));
    assert_preparation_failure(&events, ControlPhase::Quiesce, PreparationStep::Deadline);
    assert_served_by(&quiesce, 1);
    quiesce.shutdown();

    let mut activate = Harness::launch_with_executor("normal", FaultExecutor::default());
    activate
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Candidate,
            ControlPhase::Activate,
            PreparationStep::Encoding,
        ));
    activate.replace("normal", 2);
    let events = activate.until(|master| master.state() == MasterState::Running);
    assert_preparation_failure(&events, ControlPhase::Activate, PreparationStep::Encoding);
    assert_eq!(activate.master.active_instance().unwrap().as_str(), "a");
    activate.shutdown();

    let mut drain = Harness::launch_with_executor("normal", FaultExecutor::default());
    drain.master.action_executor_mut().fail(FaultPoint::Prepare(
        WorkerRole::Retired,
        ControlPhase::Drain,
        PreparationStep::Allocation,
    ));
    drain.replace("normal", 2);
    let events = drain.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "b")
    });
    assert_preparation_failure(&events, ControlPhase::Drain, PreparationStep::Allocation);
    assert_served_by(&drain, 2);
    drain.shutdown();

    let mut retired_shutdown = Harness::launch_with_executor("normal", FaultExecutor::default());
    retired_shutdown
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Retired,
            ControlPhase::Shutdown,
            PreparationStep::RequestId,
        ));
    retired_shutdown.replace("normal", 2);
    let events = retired_shutdown.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "b")
    });
    assert_preparation_failure(&events, ControlPhase::Shutdown, PreparationStep::RequestId);
    retired_shutdown.shutdown();

    let mut reactivate = Harness::launch_with_executor("normal", FaultExecutor::default());
    reactivate
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Active,
            ControlPhase::Reactivate,
            PreparationStep::Deadline,
        ));
    reactivate.replace("reject-activate", 2);
    let events = reactivate.until(|master| master.state() == MasterState::Failed);
    assert_preparation_failure(&events, ControlPhase::Reactivate, PreparationStep::Deadline);

    let mut shutdown = Harness::launch_with_executor("normal", FaultExecutor::default());
    shutdown
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Active,
            ControlPhase::Shutdown,
            PreparationStep::Encoding,
        ));
    assert_eq!(
        shutdown.master.shutdown(Instant::now()).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: true,
        }
    );
    let events = shutdown.until(|master| master.state() == MasterState::Stopped);
    assert_preparation_failure(&events, ControlPhase::Shutdown, PreparationStep::Encoding);
}

#[test]
pub(super) fn shutdown_consumes_deferred_active_exit_and_completes() {
    let mut harness = Harness::launch("crash-notice");
    harness.replace("hold-activate", 2);
    harness.until(|master| master.state() == MasterState::ActivatingCandidate);
    harness.crash_active_while_activating();

    assert_eq!(
        harness.master.shutdown(Instant::now()).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: false,
        }
    );
    let events = harness.until(|master| master.state() == MasterState::Stopped);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
    assert_eq!(
        harness.master.shutdown_progress(),
        ShutdownProgress::Complete { forced: false }
    );
}

#[test]
pub(super) fn begin_is_validated_before_spawn_and_spawn_failure_leaves_active_unchanged() {
    let mut harness = Harness::launch("normal");
    let mut failing = FailingFactory::default();
    assert!(
        harness
            .master
            .replace(&mut failing, worker("stale", 1, "normal"), Instant::now())
            .is_err()
    );
    assert_eq!(failing.calls, 0);

    assert!(
        harness
            .master
            .replace(&mut failing, worker("b", 2, "normal"), Instant::now())
            .is_err()
    );
    assert_eq!(failing.calls, 1);
    let events = harness.master.poll(Instant::now()).unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::SpawnFailed { instance_id } if instance_id.as_str() == "b"
    )));
    assert_eq!(harness.master.state(), MasterState::Running);
    assert_served_by(&harness, 1);
    harness.shutdown();
}

#[test]
pub(super) fn stable_listener_originals_are_cloexec_and_not_inherited() {
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = fs::read_link(format!("/proc/self/fd/{}", tcp.as_raw_fd())).unwrap();
    let flags = fcntl_getfd(&tcp).unwrap();
    fcntl_setfd(&tcp, flags - FdFlags::CLOEXEC).unwrap();
    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(1),
        role: DescriptorRole::Traffic(String::from("tcp")),
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp(tcp.local_addr().unwrap())),
        mode: None,
    }])
    .unwrap();
    let _listeners =
        StableListeners::new(manifest, vec![OwnedFd::from(tcp)], Arc::new(())).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_master-worker-fixture"))
        .arg("probe-fd")
        .arg(target.as_os_str())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
pub(super) fn configuration_rejects_zero_and_unbounded_timeouts() {
    assert!(
        MasterConfig::new(
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .is_err()
    );
    assert!(
        MasterConfig::new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(3_601),
        )
        .is_err()
    );
}

#[test]
pub(super) fn version_two_master_rejects_a_version_one_worker_before_spawn() {
    let listeners = StableListeners::new(
        DescriptorManifest::new(Vec::new()).unwrap(),
        Vec::new(),
        Arc::new(()),
    )
    .unwrap();
    let mut input = worker("legacy", 1, "normal");
    input.identity.protocol = 1;
    let mut factory = FailingFactory::default();

    assert!(matches!(
        Master::launch(config(), listeners, &mut factory, input, Instant::now()),
        Err(MasterError::ProtocolVersion {
            expected: 2,
            actual: 1
        })
    ));
    assert_eq!(factory.calls, 0, "legacy worker reached the spawn boundary");
}

pub(super) fn listeners() -> (
    tempfile::TempDir,
    StableListeners,
    Vec<PathBuf>,
    SocketAddr,
    PathBuf,
    KeptListeners,
) {
    let temporary = tempfile::tempdir().unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_address = tcp.local_addr().unwrap();
    let mut unix_bytes = temporary.path().as_os_str().as_bytes().to_vec();
    unix_bytes.extend_from_slice(b"/master-\xff.sock");
    let unix_path = PathBuf::from(OsString::from_vec(unix_bytes));
    let unix = UnixListener::bind(&unix_path).unwrap();
    let targets = [&tcp as &dyn AsRawFd, &unix as &dyn AsRawFd]
        .into_iter()
        .map(|listener| fs::read_link(format!("/proc/self/fd/{}", listener.as_raw_fd())).unwrap())
        .collect();
    let manifest = DescriptorManifest::new(vec![
        DescriptorSlot {
            id: SlotId(1),
            role: DescriptorRole::Traffic(String::from("tcp")),
            kind: DescriptorKind::TcpListener,
            bind: Some(BindIdentity::Tcp(tcp_address)),
            mode: None,
        },
        DescriptorSlot {
            id: SlotId(2),
            role: DescriptorRole::Traffic(String::from("unix")),
            kind: DescriptorKind::UnixListener,
            bind: Some(BindIdentity::UnixPath(unix_path.clone())),
            mode: None,
        },
    ])
    .unwrap();
    let kept = KeptListeners {
        tcp: tcp.try_clone().unwrap(),
        unix: unix.try_clone().unwrap(),
        path_guard: Arc::new(UnixPathGuard(unix_path.clone())),
    };
    let originals = vec![OwnedFd::from(tcp), OwnedFd::from(unix)];
    let listeners =
        StableListeners::new(manifest, originals, Arc::clone(&kept.path_guard)).unwrap();
    (temporary, listeners, targets, tcp_address, unix_path, kept)
}

pub(super) fn config() -> MasterConfig {
    MasterConfig::new(
        Duration::from_millis(300),
        Duration::from_millis(300),
        Duration::from_millis(300),
        Duration::from_millis(300),
        Duration::from_millis(300),
    )
    .unwrap()
}

pub(super) fn factory() -> WorkerSpawner {
    WorkerSpawner::new(
        env!("CARGO_BIN_EXE_master-launcher-fixture"),
        Duration::from_secs(2),
    )
    .unwrap()
}

pub(super) fn worker(name: &str, generation: u64, behavior: &str) -> WorkerInput<WorkerCommand> {
    let token = [u8::try_from(generation).unwrap(); 16];
    let mut encoded_token = String::with_capacity(32);
    for byte in token {
        write!(encoded_token, "{byte:02x}").unwrap();
    }
    WorkerInput {
        instance_id: InstanceId::new(name).unwrap(),
        identity: WorkerIdentity {
            instance: InstanceToken(token),
            generation: GenerationId(generation),
            protocol: CONTROL_PROTOCOL_VERSION,
        },
        command: WorkerCommand::new(env!("CARGO_BIN_EXE_master-worker-fixture"))
            .unwrap()
            .arg(behavior)
            .arg(generation.to_string())
            .arg(encoded_token),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn contained_worker(
    name: &str,
    generation: u64,
    behavior: &str,
    pid_file: &Path,
) -> WorkerInput<WorkerCommand> {
    let mut input = worker(name, generation, behavior);
    input.command = input.command.arg(pid_file.as_os_str());
    input
}

pub(super) fn descriptor_target(descriptor: &impl AsRawFd) -> PathBuf {
    fs::read_link(format!("/proc/self/fd/{}", descriptor.as_raw_fd())).unwrap()
}

/// Whether any process this user can inspect still holds a descriptor on the socket `target`
/// (a `socket:[inode]` link), which does not depend on which port a parallel test binds next.
pub(super) fn socket_is_open_in_any_process(target: &Path) -> bool {
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().as_bytes().iter().all(u8::is_ascii_digit))
        .filter_map(|entry| fs::read_dir(entry.path().join("fd")).ok())
        .flatten()
        .filter_map(Result::ok)
        .any(|descriptor| fs::read_link(descriptor.path()).is_ok_and(|link| link == target))
}

pub(super) fn matching_descriptor_count(targets: &[PathBuf]) -> usize {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|target| targets.contains(target))
        .count()
}

pub(super) fn poll_until<E: ActionExecutor>(
    master: &mut Master<E>,
    mut predicate: impl FnMut(&Master<E>) -> bool,
) -> Vec<MasterEvent> {
    let started = Instant::now();
    let mut events = Vec::new();
    while !predicate(master) {
        events.extend(master.poll(Instant::now()).unwrap());
        assert!(started.elapsed() < TEST_TIMEOUT);
        thread::sleep(POLL_INTERVAL);
    }
    events.extend(master.poll(Instant::now()).unwrap());
    events
}

pub(super) fn assert_served_by<E: ActionExecutor>(harness: &Harness<E>, generation: u64) {
    for _ in 0..5 {
        assert_eq!(probe_tcp(harness.tcp_address), generation);
        assert_eq!(probe_unix(&harness.unix_path), generation);
    }
}

pub(super) fn assert_preparation_failure(
    events: &[MasterEvent],
    phase: ControlPhase,
    step: PreparationStep,
) {
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::PreparationFailed {
            phase: actual_phase,
            step: actual_step,
            ..
        } if *actual_phase == phase && *actual_step == step
    )));
}

pub(super) fn assert_shutdown_from_phase(mut harness: Harness, phase: MasterState) {
    assert_eq!(harness.master.state(), phase);
    assert!(matches!(
        harness.master.shutdown(Instant::now()).unwrap(),
        ShutdownProgress::Pending { .. }
    ));
    assert_eq!(harness.master.state(), MasterState::ShuttingDown);
    let events = harness.until(|master| master.state() == MasterState::Stopped);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
}

pub(super) fn replacement_event_sequence(events: &[MasterEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            MasterEvent::ReplacementStarted { .. } => Some("started"),
            MasterEvent::ReplacementCommitted { .. } => Some("committed"),
            MasterEvent::TerminationRequested {
                role: WorkerRole::Retired,
                ..
            } => Some("retired-termination-requested"),
            MasterEvent::WorkerExited {
                role: WorkerRole::Retired,
                ..
            } => Some("retired-exited"),
            MasterEvent::ReplacementCompleted { .. } => Some("completed"),
            _ => None,
        })
        .collect()
}

pub(super) fn rollback_event_sequence(events: &[MasterEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            MasterEvent::ReplacementStarted { .. } => Some("started"),
            MasterEvent::RollbackStarted { .. } => Some("rollback-started"),
            MasterEvent::TerminationRequested {
                role: WorkerRole::Candidate,
                ..
            } => Some("candidate-termination-requested"),
            MasterEvent::WorkerExited {
                role: WorkerRole::Candidate,
                ..
            } => Some("candidate-exited"),
            MasterEvent::RollbackCompleted { .. } => Some("rollback-completed"),
            _ => None,
        })
        .collect()
}

pub(super) fn probe_tcp(address: SocketAddr) -> u64 {
    let started = Instant::now();
    loop {
        if let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(50)) {
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut response = [0_u8; 8];
            if stream.read_exact(&mut response).is_ok() {
                return u64::from_be_bytes(response);
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

pub(super) fn probe_unix(path: &PathBuf) -> u64 {
    let started = Instant::now();
    loop {
        if let Ok(mut stream) = UnixStream::connect(path) {
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut response = [0_u8; 8];
            if stream.read_exact(&mut response).is_ok() {
                return u64::from_be_bytes(response);
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

pub(super) fn wait_process_gone(pid: u32) {
    let path = PathBuf::from(format!("/proc/{pid}"));
    let started = Instant::now();
    while path.exists() && started.elapsed() < Duration::from_secs(2) {
        thread::sleep(POLL_INTERVAL);
    }
    assert!(!path.exists(), "process {pid} was not reaped");
}

#[cfg(target_os = "linux")]
pub(super) fn wait_for_pids(path: &Path) -> Vec<u32> {
    let started = Instant::now();
    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            let pids = contents
                .lines()
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()
                .expect("descendant pids");
            if !pids.is_empty() {
                return pids;
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(target_os = "linux")]
pub(super) fn worker_cgroup_path(pid: u32) -> PathBuf {
    let membership = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .expect("unified worker cgroup");
    Path::new(DEFAULT_CGROUP_V2_ROOT).join(relative.trim_start_matches('/'))
}

#[cfg(target_os = "linux")]
pub(super) fn assert_cgroup_released(path: &Path) {
    let started = Instant::now();
    while path.exists() && started.elapsed() < Duration::from_secs(2) {
        if fs::read_to_string(path.join("cgroup.procs")).is_ok_and(|pids| pids.trim().is_empty()) {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    if path.exists() {
        assert!(
            fs::read_to_string(path.join("cgroup.procs")).is_ok_and(|pids| pids.trim().is_empty()),
            "worker cgroup remained populated: {}",
            path.display()
        );
        let started = Instant::now();
        while path.exists() && started.elapsed() < Duration::from_secs(2) {
            thread::sleep(POLL_INTERVAL);
        }
    }
    assert!(
        !path.exists(),
        "worker cgroup survived cleanup: {}",
        path.display()
    );
}
