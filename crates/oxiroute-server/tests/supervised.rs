#![cfg(target_os = "linux")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use oxiroute_server::config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome};
use oxiroute_supervision::{GenerationId, InstanceId};
use oxiroute_supervision_unix::{DescriptorManifest, InstanceToken};
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, Master, MasterConfig, MasterState, ShutdownProgress, StableListeners,
    WorkerInput, WorkerRole,
};
use oxiroute_supervisor_process::{WorkerCommand, WorkerIdentity, WorkerSpawner};
use rustix::fs::OFlags;

const MARKER: &str = "--__oxiroute-worker-7f3c9d1e";
const TEST_RUNTIME_FAILURE_ENV: &str = "OXIROUTE_INTERNAL_TEST_RUNTIME_FAILURE";
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN: [u8; 16] = [0x51; 16];

struct Harness {
    master: Master,
    worker_pid: u32,
    launcher_pid: u32,
}

impl Harness {
    fn launch(revision: String, inject_runtime_failure: bool) -> Self {
        let mut factory = WorkerSpawner::new(
            env!("CARGO_BIN_EXE_oxiroute-supervisor-launcher-fixture"),
            Duration::from_secs(5),
        )
        .expect("production launcher implementation");
        let identity = WorkerIdentity {
            instance: InstanceToken(TOKEN),
            generation: GenerationId(1),
            protocol: CONTROL_PROTOCOL_VERSION,
        };
        let mut command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
            .expect("real oxiroute worker")
            .arg(MARKER)
            .arg(identity.generation.to_string())
            .arg(encode_token(TOKEN))
            .arg(config_path())
            .arg(revision);
        if inject_runtime_failure {
            command = command.env(TEST_RUNTIME_FAILURE_ENV, "1");
        }
        let listeners = StableListeners::new(
            DescriptorManifest::new(Vec::new()).expect("empty manifest"),
            Vec::new(),
        )
        .expect("empty stable listeners");
        let master = Master::launch(
            MasterConfig::new(
                Duration::from_secs(10),
                Duration::from_secs(1),
                Duration::from_secs(10),
                Duration::from_secs(1),
                Duration::from_secs(6),
            )
            .expect("master deadlines"),
            listeners,
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-stage-1").expect("instance identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("launch supervised worker");
        let worker_pid = master
            .worker_id(WorkerRole::Active)
            .expect("initial worker pid");
        let launcher_pid = master
            .worker_process_group_id(WorkerRole::Active)
            .expect("launcher process group");
        Self {
            master,
            worker_pid,
            launcher_pid,
        }
    }

    fn poll_until(&mut self, expected: MasterState) {
        let started = Instant::now();
        while self.master.state() != expected {
            self.master.poll(Instant::now()).expect("master poll");
            assert!(
                started.elapsed() < TEST_TIMEOUT,
                "master remained in {:?}",
                self.master.state()
            );
            thread::sleep(POLL_INTERVAL);
        }
        self.master.poll(Instant::now()).expect("final master poll");
    }

    fn verify_reaped(&self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        let worker = PathBuf::from(format!("/proc/{}/", self.worker_pid));
        let launcher = PathBuf::from(format!("/proc/{}/", self.launcher_pid));
        while (worker.exists() || launcher.exists()) && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        assert!(
            !worker.exists(),
            "worker remained after terminal master state"
        );
        assert!(
            !launcher.exists(),
            "launcher remained after terminal master state"
        );
    }
}

#[test]
fn internal_worker_marker_is_absent_from_cli_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .arg("--help")
        .output()
        .expect("CLI help");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(MARKER));
}

#[test]
fn malformed_reserved_worker_marker_fails_closed_before_clap() {
    let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .arg("--__oxiroute-worker-invalid")
        .output()
        .expect("malformed internal invocation");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid reserved worker marker"));
    assert!(!stderr.contains("Usage:"));
}

#[test]
fn supervised_worker_rejects_a_revision_mismatch_and_is_reaped() {
    let mut harness = Harness::launch("0".repeat(64), false);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
}

#[test]
fn supervised_worker_adopts_marker_activates_and_shuts_down_boundedly() {
    let mut harness = Harness::launch(canonical_revision(), false);
    harness.poll_until(MasterState::Running);
    verify_control_descriptor(harness.worker_pid);
    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
fn generation_runtime_death_closes_control_and_is_reaped_without_killing_the_worker_in_test() {
    let mut harness = Harness::launch(canonical_revision(), true);
    harness.poll_until(MasterState::Running);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
}

fn verify_control_descriptor(worker_pid: u32) {
    let descriptor_root = PathBuf::from(format!("/proc/{worker_pid}/fd"));
    assert_eq!(
        fs::read_link(descriptor_root.join("0")).expect("worker fd 0"),
        Path::new("/dev/null")
    );
    let sockets = fs::read_dir(&descriptor_root)
        .expect("worker descriptors")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            fs::read_link(entry.path())
                .ok()
                .and_then(|target| {
                    target
                        .to_string_lossy()
                        .strip_prefix("socket:[")
                        .and_then(|target| target.strip_suffix(']'))
                        .map(str::to_owned)
                })
                .map(|inode| (entry.file_name(), inode))
        })
        .collect::<Vec<_>>();
    assert!(!sockets.is_empty(), "worker had no control socket");
    let unix_sockets = fs::read_to_string(format!("/proc/{worker_pid}/net/unix"))
        .expect("worker Unix socket table");
    let control = sockets
        .iter()
        .filter(|(_, inode)| {
            unix_sockets.lines().any(|line| {
                let columns = line.split_whitespace().collect::<Vec<_>>();
                columns.get(4) == Some(&"0005") && columns.get(6) == Some(&inode.as_str())
            })
        })
        .map(|(descriptor, _)| descriptor)
        .collect::<Vec<_>>();
    assert_eq!(control.len(), 1, "expected one SOCK_SEQPACKET control fd");
    let details =
        fs::read_to_string(PathBuf::from(format!("/proc/{worker_pid}/fdinfo")).join(control[0]))
            .expect("worker control flags");
    let flags = details
        .lines()
        .find_map(|line| line.strip_prefix("flags:\t"))
        .and_then(|value| u64::from_str_radix(value, 8).ok())
        .expect("parse worker control flags");
    assert_ne!(flags & u64::from(OFlags::CLOEXEC.bits()), 0);
}

fn encode_token(token: [u8; 16]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(32);
    for byte in token {
        write!(encoded, "{byte:02x}").expect("write token");
    }
    encoded
}

fn canonical_revision() -> String {
    let coordinator = CanonicalConfigCoordinator::new(config_path()).expect("fixture coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("canonical fixture was rejected");
    };
    document.candidate_revision.to_string()
}

fn config_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/supervised-empty.kdl"
    ))
}
