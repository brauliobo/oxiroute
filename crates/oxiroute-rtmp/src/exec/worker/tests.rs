use std::{path::PathBuf, sync::Arc, thread, time::Duration};

use crate::ExecLimits;

use super::*;

fn limits(max_queue_messages: usize, max_queue_bytes: usize) -> ExecLimits {
    ExecLimits::new(
        max_queue_messages,
        max_queue_bytes,
        64 * 1024,
        64 * 1024,
        Duration::from_secs(5),
        Duration::from_secs(1),
        1,
        Duration::from_millis(10),
        1,
    )
    .expect("test limits are valid")
}

fn profile(
    mode: ExecMode,
    network: ExecNetworkPolicy,
    max_queue_messages: usize,
    max_queue_bytes: usize,
) -> ExecProfile {
    ExecProfile::new(
        "capture",
        "live",
        mode,
        ExecTrigger::Publisher,
        PathBuf::from("/bin/cat"),
        Vec::<String>::new(),
        Vec::<crate::ExecEnvironment>::new(),
        PathBuf::from("/tmp"),
        crate::ExecFilesystemPolicy::WorkingDirectory,
        network,
        limits(max_queue_messages, max_queue_bytes),
        false,
    )
    .expect("test profile is valid")
}

fn respawning_profile() -> ExecProfile {
    ExecProfile::new(
        "respawn",
        "live",
        ExecMode::Command,
        ExecTrigger::Publisher,
        PathBuf::from("/bin/false"),
        Vec::<String>::new(),
        Vec::<crate::ExecEnvironment>::new(),
        PathBuf::from("/tmp"),
        crate::ExecFilesystemPolicy::WorkingDirectory,
        ExecNetworkPolicy::Inherited,
        limits(1, 64 * 1024),
        true,
    )
    .expect("respawning test profile is valid")
}

fn command_profile(
    executable: &str,
    arguments: impl IntoIterator<Item = String>,
    limits: ExecLimits,
) -> ExecProfile {
    ExecProfile::new(
        "command",
        "live",
        ExecMode::Command,
        ExecTrigger::Publisher,
        PathBuf::from(executable),
        arguments,
        Vec::<crate::ExecEnvironment>::new(),
        PathBuf::from("/tmp"),
        crate::ExecFilesystemPolicy::WorkingDirectory,
        ExecNetworkPolicy::Inherited,
        limits,
        false,
    )
    .expect("command test profile is valid")
}

fn wait_for(
    worker: &ExecWorker,
    predicate: impl Fn(&ExecWorkerStatus) -> bool,
) -> ExecWorkerStatus {
    for _ in 0..200 {
        let status = worker.status();
        if predicate(&status) {
            return status;
        }
        thread::sleep(Duration::from_millis(5));
    }
    worker.status()
}

fn key() -> StreamKey {
    StreamKey::new("service", "live", "stream")
}

#[test]
fn inherited_transcode_worker_receives_bounded_media_frames() {
    let profiles = ExecProfileSet::new([profile(
        ExecMode::Transcode,
        ExecNetworkPolicy::Inherited,
        8,
        64 * 1024,
    )])
    .expect("profile set");
    let mut workers = profiles.start_publisher("service", &key(), SessionId::new());
    assert_eq!(workers.len(), 1);
    let status = wait_for(&workers[0], |status| {
        status.phase == ExecWorkerPhase::Running
    });
    assert_eq!(status.phase, ExecWorkerPhase::Running);

    let event =
        MediaEvent::audio(10, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).expect("audio event");
    assert_eq!(workers[0].try_enqueue(&event), ExecEnqueueResult::Queued);
    let status = wait_for(&workers[0], |status| {
        status.stdin_bytes > 0 && status.stdout_bytes > 0
    });
    assert!(status.stdin_bytes >= event.payload_len() + 9);
    assert!(status.stdout_bytes >= event.payload_len() + 9);
    workers.clear();
}

#[test]
fn queue_bytes_fail_closed_without_waiting_for_the_child() {
    let profiles = ExecProfileSet::new([profile(
        ExecMode::Transcode,
        ExecNetworkPolicy::Inherited,
        1,
        1,
    )])
    .expect("profile set");
    let workers = profiles.start_publisher("service", &key(), SessionId::new());
    assert_eq!(workers.len(), 1);
    let event =
        MediaEvent::audio(10, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).expect("audio event");
    assert_eq!(workers[0].try_enqueue(&event), ExecEnqueueResult::Dropped);
}

#[test]
fn profile_process_admission_is_bounded() {
    let profiles = ExecProfileSet::new([profile(
        ExecMode::Command,
        ExecNetworkPolicy::Inherited,
        1,
        64 * 1024,
    )])
    .expect("profile set");
    let first = profiles.start_publisher("service", &key(), SessionId::new());
    assert_eq!(first.len(), 1);
    let second = profiles.start_publisher("service", &key(), SessionId::new());
    assert!(second.is_empty());
}

#[test]
fn disabled_network_workers_fail_closed() {
    let profiles = ExecProfileSet::new([profile(
        ExecMode::Command,
        ExecNetworkPolicy::Disabled,
        1,
        64 * 1024,
    )])
    .expect("profile set");
    let workers = profiles.start_publisher("service", &key(), SessionId::new());
    let status = wait_for(&workers[0], |status| {
        matches!(
            status.phase,
            ExecWorkerPhase::Failed | ExecWorkerPhase::Running
        )
    });
    if status.phase == ExecWorkerPhase::Failed {
        assert_eq!(
            status.failure,
            Some(ExecWorkerFailure::NetworkIsolationUnavailable)
        );
    }
}

#[test]
fn respawn_count_is_bounded_and_reported() {
    let profiles = ExecProfileSet::new([respawning_profile()]).expect("profile set");
    let workers = profiles.start_publisher("service", &key(), SessionId::new());
    let status = wait_for(&workers[0], |status| {
        status.phase == ExecWorkerPhase::Failed
    });
    assert_eq!(status.respawns, 1);
    assert_eq!(status.failure, Some(ExecWorkerFailure::RespawnLimit));
}

#[test]
fn missing_executable_is_reported_as_a_worker_failure() {
    let profile = command_profile(
        "/definitely/not/an/oxiroute-executable",
        Vec::<String>::new(),
        limits(1, 64 * 1024),
    );
    let profiles = ExecProfileSet::new([profile]).expect("profile set");
    let workers = profiles.start_publisher("service", &key(), SessionId::new());
    let status = wait_for(&workers[0], |status| {
        status.phase == ExecWorkerPhase::Failed
    });
    assert_eq!(status.failure, Some(ExecWorkerFailure::SpawnFailed));
    assert_eq!(status.respawns, 0);
}

#[test]
fn output_limit_is_reported_even_when_the_child_exits_immediately() {
    let limits = ExecLimits::new(
        1,
        64 * 1024,
        4,
        64 * 1024,
        Duration::from_secs(5),
        Duration::from_secs(1),
        1,
        Duration::from_millis(10),
        0,
    )
    .expect("output-limit test limits are valid");
    let profile = command_profile("/bin/printf", ["%s".into(), "0123456789".into()], limits);
    let profiles = ExecProfileSet::new([profile]).expect("profile set");
    let workers = profiles.start_publisher("service", &key(), SessionId::new());
    let status = wait_for(&workers[0], |status| {
        status.phase == ExecWorkerPhase::Failed
    });
    assert_eq!(status.failure, Some(ExecWorkerFailure::OutputLimit));
    assert!(status.output_truncated);
    assert_eq!(status.stdout_bytes, 4);
}
