#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn supervised_worker_replaces_a_same_manifest_generation() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = ports_support::unused_address();
    let unix_path = directory.path().join("replacement.sock");
    let initial = listeners_only_config(tcp_address, unix_path.clone());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &initial.clone().validate().expect("valid initial config"),
        ),
    )
    .expect("write initial config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);

    let mut updated = initial;
    let HttpRouteAction::FixedResponse { body, .. } =
        &mut updated.http_services[0].routes[0].action
    else {
        panic!("fixture route is not a fixed response");
    };
    *body = "stage-3".into();
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &updated
                .clone()
                .validate()
                .expect("valid replacement config"),
        ),
    )
    .expect("write replacement config");

    let revision = canonical_revision(&path);
    let identity = WorkerIdentity {
        instance: InstanceToken([0x52; 16]),
        generation: GenerationId(2),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
        .expect("real oxiroute worker")
        .arg(MARKER)
        .arg(identity.generation.to_string())
        .arg(encode_token([0x52; 16]))
        .arg(&path)
        .arg(revision);
    let mut factory = WorkerSpawner::new(supervisor_launcher(), TEST_TIMEOUT)
        .expect("production launcher implementation");
    harness
        .master
        .replace(
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-stage-2-candidate")
                    .expect("candidate identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("start replacement");

    let started = Instant::now();
    let mut committed = false;
    while !committed || harness.master.state() != MasterState::Running {
        let events = harness.master.poll(Instant::now()).expect("master poll");
        committed |= events
            .iter()
            .any(|event| matches!(event, MasterEvent::ReplacementCommitted { .. }));
        assert!(started.elapsed() < TEST_TIMEOUT, "replacement timed out");
        thread::sleep(POLL_INTERVAL);
    }
    let mut stream = TcpStream::connect(tcp_address).expect("replacement TCP connection");
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .expect("TCP read timeout");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write replacement request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read replacement response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("stage-3"), "{response}");

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
pub(super) fn supervised_worker_reactivates_after_a_replacement_rejection() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = ports_support::unused_address();
    let unix_path = directory.path().join("rollback.sock");
    let config = listeners_only_config(tcp_address, unix_path);
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid rollback config"),
        ),
    )
    .expect("write rollback config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);

    let identity = WorkerIdentity {
        instance: InstanceToken([0x53; 16]),
        generation: GenerationId(2),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
        .expect("real oxiroute worker")
        .arg(MARKER)
        .arg(identity.generation.to_string())
        .arg(encode_token([0x53; 16]))
        .arg(&path)
        .arg("0".repeat(64));
    let mut factory = WorkerSpawner::new(supervisor_launcher(), TEST_TIMEOUT)
        .expect("production launcher implementation");
    harness
        .master
        .replace(
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-stage-2-rollback")
                    .expect("candidate identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("start rejected replacement");

    let started = Instant::now();
    let mut rolled_back = false;
    while !rolled_back || harness.master.state() != MasterState::Running {
        let events = harness.master.poll(Instant::now()).expect("master poll");
        rolled_back |= events
            .iter()
            .any(|event| matches!(event, MasterEvent::RollbackCompleted { .. }));
        assert!(started.elapsed() < TEST_TIMEOUT, "rollback timed out");
        thread::sleep(POLL_INTERVAL);
    }
    assert_fixed_response(TcpStream::connect(tcp_address).expect("rollback TCP connection"));

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
pub(super) fn listener_duplication_failure_never_acknowledges_adoption_or_reaches_running() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = ports_support::unused_address();
    let unix_path = directory.path().join("failure.sock");
    let config = listeners_only_config(tcp_address, unix_path);
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid supervised config"),
        ),
    )
    .expect("write supervised config");
    let mut harness = Harness::launch_at_with_failures(
        &path,
        canonical_revision(&path),
        false,
        Some(TEST_LISTENER_DUPLICATION_FAILURE_ENV),
    );
    let started = Instant::now();
    while harness.master.state() != MasterState::Failed {
        let state = harness.master.state();
        assert!(
            !matches!(state, MasterState::ActivatingInitial | MasterState::Running),
            "failed listener startup advanced to {state:?}"
        );
        harness.master.poll(Instant::now()).expect("master poll");
        assert!(started.elapsed() < TEST_TIMEOUT);
        thread::sleep(POLL_INTERVAL);
    }
    harness.verify_reaped();
}

#[test]
pub(super) fn supervised_listener_rejection_releases_ownership_and_allows_clean_retry() {
    let directory = tempfile::tempdir().expect("supervised retry fixture directory");
    let tcp_address = ports_support::unused_address();
    let unix_path = directory.path().join("retry.sock");
    let config = listeners_only_config(tcp_address, unix_path.clone());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid retry config"),
        ),
    )
    .expect("write retry config");

    let mut rejected = Harness::launch_at_with_failures(
        &path,
        canonical_revision(&path),
        false,
        Some(TEST_LISTENER_DUPLICATION_FAILURE_ENV),
    );
    rejected.poll_until(MasterState::Failed);
    rejected.verify_reaped();
    drop(rejected);
    assert!(
        !unix_path.exists(),
        "rejected master retained Unix ownership"
    );

    let mut retry = Harness::launch_at(&path, canonical_revision(&path), false);
    retry.poll_until(MasterState::Running);
    assert_fixed_response(TcpStream::connect(tcp_address).expect("retry TCP connection"));
    retry
        .master
        .shutdown(Instant::now())
        .expect("retry shutdown");
    retry.poll_until(MasterState::Stopped);
    retry.verify_reaped();
}

#[test]
pub(super) fn supervised_listener_worker_crash_is_reaped_and_master_cleans_namespace() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = ports_support::unused_address();
    let unix_path = directory.path().join("crash.sock");
    let config = listeners_only_config(tcp_address, unix_path.clone());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid supervised config"),
        ),
    )
    .expect("write supervised config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), true);

    harness.poll_until(MasterState::Running);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
    drop(harness);
    assert!(
        !unix_path.exists(),
        "failed master did not clean up Unix socket"
    );
}
