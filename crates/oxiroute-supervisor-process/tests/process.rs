use std::{
    fs,
    os::{
        fd::{AsFd, AsRawFd},
        unix::process::ExitStatusExt,
    },
    process::{Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::InstanceToken;
use oxiroute_supervisor_process::{
    AuthenticatedChannelError, DEFAULT_CGROUP_V2_ROOT, ExecutableError,
    MAX_WORKER_METADATA_ITEM_BYTES, SpawnError, WorkerCommand, WorkerEvent, WorkerIdentity,
    WorkerMetadataError, WorkerProcess, WorkerSpawner, probe_cgroup_v2,
};
use rustix::{
    io::{FdFlags, fcntl_getfd, fcntl_setfd},
    process::{Pid, Signal, kill_process, test_kill_process},
};

const INSTANCE: InstanceToken = InstanceToken(*b"process-worker01");
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

fn identity() -> WorkerIdentity {
    WorkerIdentity {
        instance: INSTANCE,
        generation: GenerationId(11),
        protocol: 7,
    }
}

fn spawner(timeout: Duration) -> WorkerSpawner {
    WorkerSpawner::new(env!("CARGO_BIN_EXE_oxiroute-worker-launcher"), timeout).unwrap()
}

fn command(mode: &str) -> WorkerCommand {
    WorkerCommand::new(env!("CARGO_BIN_EXE_worker-process-fixture"))
        .unwrap()
        .arg(mode)
        .env("OXIROUTE_FIXTURE_METADATA", "non-secret")
}

fn spawn(mode: &str) -> Result<WorkerProcess, SpawnError> {
    spawner(HANDSHAKE_TIMEOUT).spawn(command(mode), identity())
}

fn exit_status(event: Option<WorkerEvent>) -> ExitStatus {
    let Some(WorkerEvent::ProcessGroupExited(status)) = event else {
        panic!("missing exit event");
    };
    status
}

fn wait_for_file(path: &std::path::Path) -> String {
    let started = Instant::now();
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && !value.trim().is_empty()
        {
            return value;
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(5));
    }
}

fn process_exists(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pid| test_kill_process(pid).is_ok())
}

fn assert_process_gone(pid: u32) {
    let started = Instant::now();
    while process_exists(pid) && started.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!process_exists(pid), "process {pid} survived group signal");
}

#[test]
fn authenticated_worker_starts_and_reports_exit_once() {
    let mut worker = spawn("success").unwrap();
    assert!(worker.id() > 0);
    assert!(worker.process_group_id().is_some());
    assert_ne!(worker.process_group_id(), Some(worker.id()));
    let status = exit_status(worker.wait_event().unwrap());
    assert_eq!(status.signal(), Some(9));
    assert_eq!(worker.process_group_id(), None);
    assert_eq!(worker.poll_event().unwrap(), None);
}

#[test]
fn executable_resolution_and_environment_are_sanitized() {
    assert!(matches!(
        WorkerCommand::new("oxiroute-no-such-worker"),
        Err(ExecutableError::NotFound { .. })
    ));
    let mut worker = spawner(HANDSHAKE_TIMEOUT)
        .spawn(
            command("environment")
                .env("CONFIGURED_MODE", "fixture")
                .env("LD_PRELOAD", "/definitely/not/a/preload-library.so")
                .env("OXIROUTE_AUDIT_DIR", "/var/lib/oxiroute/audit")
                .env("OXIROUTE_AUDIT_MAX_FILE_BYTES", "1048576")
                .inherit_env("HOME"),
            identity(),
        )
        .unwrap();
    let status = exit_status(worker.wait_event().unwrap());
    assert_eq!(status.signal(), Some(9));

    let oversized = "x".repeat(MAX_WORKER_METADATA_ITEM_BYTES + 1);
    assert!(matches!(
        spawner(HANDSHAKE_TIMEOUT)
            .spawn(command("success").env("OVERSIZED", oversized), identity()),
        Err(SpawnError::Metadata(
            WorkerMetadataError::ItemTooLarge { .. }
        ))
    ));
}

#[test]
fn required_cgroup_containment_fails_before_spawn_without_disclosing_the_root() {
    let directory = tempfile::tempdir().unwrap();
    let result = spawner(HANDSHAKE_TIMEOUT)
        .with_cgroup_root(directory.path())
        .spawn(command("success").require_cgroup_containment(), identity());

    assert!(matches!(
        result,
        Err(SpawnError::CgroupContainmentUnavailable(
            oxiroute_supervisor_process::CgroupV2ProbeStatus::Unavailable
        ))
    ));
    let error = result.unwrap_err().to_string();
    assert!(!error.contains(directory.path().to_string_lossy().as_ref()));
}

#[test]
fn launcher_removes_parent_inheritable_sentinel_descriptor() {
    let (_reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let flags = fcntl_getfd(&writer).unwrap();
    fcntl_setfd(&writer, flags - FdFlags::CLOEXEC).unwrap();
    assert!(!fcntl_getfd(&writer).unwrap().contains(FdFlags::CLOEXEC));
    let target = fs::read_link(format!("/proc/self/fd/{}", writer.as_fd().as_raw_fd())).unwrap();
    let mut worker = spawner(HANDSHAKE_TIMEOUT)
        .spawn(
            command("sentinel").env("SENTINEL_TARGET", target.as_os_str()),
            identity(),
        )
        .unwrap();
    let status = exit_status(worker.wait_event().unwrap());
    assert_eq!(status.signal(), Some(9));
}

#[test]
fn startup_rejects_nonce_generation_protocol_and_instance_mismatches() {
    assert!(matches!(
        spawn("wrong-nonce"),
        Err(SpawnError::NonceMismatch)
    ));
    assert!(matches!(
        spawn("wrong-generation"),
        Err(SpawnError::GenerationMismatch { .. })
    ));
    assert!(matches!(
        spawn("wrong-protocol"),
        Err(SpawnError::ProtocolMismatch { .. })
    ));
    assert!(matches!(
        spawn("wrong-instance"),
        Err(SpawnError::InstanceMismatch)
    ));
}

#[test]
fn version_two_handshake_rejects_a_version_one_worker() {
    let identity = WorkerIdentity {
        instance: INSTANCE,
        generation: GenerationId(11),
        protocol: 2,
    };
    assert!(matches!(
        spawner(HANDSHAKE_TIMEOUT).spawn(command("legacy-v1"), identity),
        Err(SpawnError::ProtocolMismatch {
            expected: 2,
            actual: 1
        })
    ));
}

#[test]
fn handshake_timeout_is_bounded_and_reaps_in_background() {
    let started = Instant::now();
    assert!(matches!(
        spawner(Duration::from_millis(100)).spawn(command("timeout"), identity()),
        Err(SpawnError::HandshakeTimeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn early_exit_and_crash_are_reported_with_status() {
    let result = spawn("early-exit");
    let Err(SpawnError::EarlyExit(exit)) = result else {
        panic!("expected early exit, got {result:?}");
    };
    assert_eq!(exit.signal(), Some(9));

    let Err(SpawnError::EarlyExit(crash)) = spawn("crash") else {
        panic!("expected crash");
    };
    assert_eq!(crash.signal(), Some(9));
}

#[test]
fn startup_credentials_must_belong_to_the_direct_child() {
    assert!(matches!(
        spawn("credential-mismatch"),
        Err(SpawnError::WorkerParentMismatch { .. })
    ));
}

#[test]
fn every_post_ready_frame_is_authenticated() {
    let mut worker = spawn("message").unwrap();
    let frame = worker.channel().receive().unwrap();
    assert_eq!(frame.payload(), b"authenticated payload");
    assert_eq!(
        frame.peer_identity().pid(),
        i32::try_from(worker.id()).unwrap()
    );
    worker.kill().unwrap();

    let mut worker = spawn("post-ready-grandchild").unwrap();
    let result = worker.channel().receive();
    assert!(
        matches!(
            result,
            Err(AuthenticatedChannelError::CredentialMismatch { .. })
        ),
        "unexpected grandchild result: {result:?}"
    );
    worker.kill().unwrap();
}

#[test]
fn nonblocking_receive_returns_when_no_frame_is_available() {
    let mut worker = spawn("linger").unwrap();
    let started = Instant::now();
    assert!(worker.channel().try_receive().unwrap().is_none());
    assert!(started.elapsed() < Duration::from_millis(100));
    worker.kill().unwrap();
}

#[test]
fn kill_request_is_nonblocking_and_reaping_remains_poll_driven() {
    let mut worker = spawn("linger").unwrap();
    let started = Instant::now();
    worker.request_kill().unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(worker.process_group_id().is_some());

    let started = Instant::now();
    loop {
        if worker.poll_event().unwrap().is_some() {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(worker.process_group_id(), None);
}

#[test]
fn channel_closes_when_direct_child_exits() {
    let mut worker = spawn("success").unwrap();
    assert!(matches!(
        worker.channel().receive(),
        Err(AuthenticatedChannelError::WorkerGroupExited(_))
    ));
    assert!(worker.poll_event().unwrap().is_some());
}

#[test]
fn drop_is_bounded_and_background_reaper_collects_the_child() {
    let workers = (0..4).map(|_| spawn("linger").unwrap()).collect::<Vec<_>>();
    let pids = workers.iter().map(WorkerProcess::id).collect::<Vec<_>>();
    let started = Instant::now();
    drop(workers);
    assert!(started.elapsed() < Duration::from_secs(1));
    for pid in pids {
        assert_process_gone(pid);
    }
    let reapers = fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_to_string(entry.path().join("comm")).ok())
        .filter(|name| name.trim().starts_with("oxiroute-worker"))
        .count();
    assert_eq!(reapers, 1);
}

#[test]
fn reaping_leader_permanently_disables_group_signaling() {
    let mut worker = spawn("success").unwrap();
    assert!(worker.process_group_id().is_some());
    worker.wait_event().unwrap();
    assert_eq!(worker.process_group_id(), None);

    let mut unrelated = Command::new(env!("CARGO_BIN_EXE_worker-process-fixture"))
        .arg("descendant-sleep")
        .spawn()
        .unwrap();
    assert_eq!(worker.kill().unwrap(), None);
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[test]
fn launcher_cleans_ordinary_descendants_after_worker_exit() {
    let temporary = tempfile::tempdir().unwrap();
    let pid_file = temporary.path().join("natural-descendant.pid");
    let mut worker = spawner(HANDSHAKE_TIMEOUT)
        .spawn(
            command("descendant-exit").env("DESCENDANT_PID_FILE", pid_file.as_os_str()),
            identity(),
        )
        .unwrap();
    let descendant = wait_for_file(&pid_file).parse::<u32>().unwrap();
    worker.wait_event().unwrap();
    assert_eq!(worker.process_group_id(), None);
    assert_process_gone(descendant);
}

#[cfg(target_os = "linux")]
#[test]
fn delegated_cgroup_cleans_a_descendant_that_escaped_the_process_group() {
    if !probe_cgroup_v2().is_ready() {
        return;
    }

    let temporary = tempfile::tempdir().unwrap();
    let pid_file = temporary.path().join("escaped-descendant.pid");
    let mut worker = spawner(HANDSHAKE_TIMEOUT)
        .spawn(
            command("escaped-descendant-exit").env("DESCENDANT_PID_FILE", pid_file.as_os_str()),
            identity(),
        )
        .unwrap();
    let cgroup_path = worker
        .cgroup_path()
        .expect("delegated worker cgroup")
        .to_owned();
    assert_eq!(
        cgroup_path.parent(),
        Some(std::path::Path::new(DEFAULT_CGROUP_V2_ROOT))
    );
    let descendant = wait_for_file(&pid_file).parse::<u32>().unwrap();

    worker.wait_event().unwrap();
    assert_process_gone(descendant);
    let started = Instant::now();
    while cgroup_path.exists() && started.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!cgroup_path.exists(), "worker cgroup survived cleanup");
}

#[cfg(target_os = "linux")]
#[test]
fn delegated_cgroup_owner_cleans_after_launcher_crash() {
    if !probe_cgroup_v2().is_ready() {
        return;
    }

    let temporary = tempfile::tempdir().unwrap();
    let pid_file = temporary.path().join("launcher-crash-descendant.pid");
    let mut worker = spawner(HANDSHAKE_TIMEOUT)
        .spawn(
            command("escaped-descendant-exit")
                .env("DESCENDANT_PID_FILE", pid_file.as_os_str())
                .env("LINGER_AFTER_DESCENDANT", "1"),
            identity(),
        )
        .unwrap();
    let cgroup_path = worker
        .cgroup_path()
        .expect("delegated worker cgroup")
        .to_owned();
    let descendant = wait_for_file(&pid_file).parse::<u32>().unwrap();
    let launcher = worker
        .process_group_id()
        .and_then(|pid| i32::try_from(pid).ok().and_then(Pid::from_raw));
    kill_process(launcher.expect("launcher pid"), Signal::KILL).unwrap();

    worker.wait_event().unwrap();
    assert_process_gone(descendant);
    let started = Instant::now();
    while cgroup_path.exists() && started.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !cgroup_path.exists(),
        "crashed launcher cgroup survived cleanup"
    );
}

#[test]
fn terminate_and_kill_signal_ordinary_descendants() {
    for terminate in [true, false] {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendant.pid");
        let mut worker = spawner(HANDSHAKE_TIMEOUT)
            .spawn(
                command("descendant").env("DESCENDANT_PID_FILE", pid_file.as_os_str()),
                identity(),
            )
            .unwrap();
        let descendant = wait_for_file(&pid_file).parse::<u32>().unwrap();
        assert!(process_exists(descendant));
        let status = if terminate {
            exit_status(worker.terminate(Duration::from_millis(100)).unwrap())
        } else {
            exit_status(worker.kill().unwrap())
        };
        assert!(matches!(status.signal(), Some(9 | 15)));
        assert_process_gone(descendant);
    }
}
