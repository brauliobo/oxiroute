#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn shutdown_deadline_kills_and_reaps_an_uncooperative_worker() {
    let harness = Harness::launch("ignore-shutdown");
    let worker_pid = harness.master.worker_id(WorkerRole::Active).unwrap();
    let launcher_pid = harness
        .master
        .worker_process_group_id(WorkerRole::Active)
        .unwrap();
    let mut harness = harness;
    let now = Instant::now();
    assert_eq!(
        harness.master.shutdown(now).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: false
        }
    );
    let deadline = harness.master.next_deadline().unwrap();
    let started = Instant::now();
    let mut events = harness.master.poll(deadline + POLL_INTERVAL).unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(matches!(
        harness.master.state(),
        MasterState::ShuttingDown | MasterState::Stopped
    ));
    if harness.master.state() == MasterState::ShuttingDown {
        assert_eq!(
            harness.master.worker_state(WorkerRole::Active),
            Some(WorkerState::Terminating { forced: true })
        );
        assert_eq!(
            harness.master.shutdown_progress(),
            ShutdownProgress::Pending {
                remaining: 1,
                forced: true
            }
        );
    }
    events.extend(harness.until(|master| master.state() == MasterState::Stopped));
    assert_eq!(
        harness.master.shutdown_progress(),
        ShutdownProgress::Complete { forced: true }
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
    wait_process_gone(worker_pid);
    wait_process_gone(launcher_pid);
}

#[cfg(target_os = "linux")]
#[test]
pub(super) fn delegated_replacement_contains_nested_and_setsid_descendants() {
    if !probe_cgroup_v2().is_ready() {
        return;
    }

    let temporary = tempfile::tempdir().unwrap();
    let active_pid_file = temporary.path().join("active-descendants.pid");
    let candidate_pid_file = temporary.path().join("candidate-descendants.pid");
    let mut harness = Harness::launch_contained("normal", &active_pid_file);
    let active_pid = harness.master.worker_id(WorkerRole::Active).unwrap();
    let active_cgroup = worker_cgroup_path(active_pid);
    let active_descendants = wait_for_pids(&active_pid_file);

    harness.replace_contained("normal", 2, &candidate_pid_file);
    harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "b")
    });
    for descendant in active_descendants {
        wait_process_gone(descendant);
    }
    assert_cgroup_released(&active_cgroup);

    let candidate_pid = harness.master.worker_id(WorkerRole::Active).unwrap();
    let candidate_cgroup = worker_cgroup_path(candidate_pid);
    let candidate_descendants = wait_for_pids(&candidate_pid_file);
    harness.shutdown();
    for descendant in candidate_descendants {
        wait_process_gone(descendant);
    }
    assert_cgroup_released(&candidate_cgroup);
}

#[cfg(target_os = "linux")]
#[test]
pub(super) fn delegated_candidate_startup_timeout_contains_nested_and_setsid_descendants() {
    if !probe_cgroup_v2().is_ready() {
        return;
    }

    let temporary = tempfile::tempdir().unwrap();
    let candidate_pid_file = temporary.path().join("candidate-descendants.pid");
    let mut harness = Harness::launch("normal");
    harness.replace_contained("timeout-adopt", 2, &candidate_pid_file);
    let candidate_pid = harness.master.worker_id(WorkerRole::Candidate).unwrap();
    let candidate_cgroup = worker_cgroup_path(candidate_pid);
    let descendants = wait_for_pids(&candidate_pid_file);
    let deadline = harness.master.next_deadline().unwrap();
    harness.master.poll(deadline + POLL_INTERVAL).unwrap();
    harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "a")
    });
    for descendant in descendants {
        wait_process_gone(descendant);
    }
    assert_cgroup_released(&candidate_cgroup);
    harness.shutdown();
}

#[cfg(target_os = "linux")]
#[test]
pub(super) fn delegated_graceful_and_forced_shutdown_contain_nested_and_setsid_descendants() {
    if !probe_cgroup_v2().is_ready() {
        return;
    }

    for behavior in ["normal", "ignore-shutdown"] {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendants.pid");
        let harness = Harness::launch_contained(behavior, &pid_file);
        let worker_pid = harness.master.worker_id(WorkerRole::Active).unwrap();
        let cgroup = worker_cgroup_path(worker_pid);
        let descendants = wait_for_pids(&pid_file);
        let events = harness.shutdown();
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::ShutdownCompleted { forced } if *forced == (behavior == "ignore-shutdown")
        )));
        for descendant in descendants {
            wait_process_gone(descendant);
        }
        assert_cgroup_released(&cgroup);
    }
}

#[test]
pub(super) fn rejected_shutdown_is_killed_and_reaped() {
    let events = Harness::launch("reject-shutdown").shutdown();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
}

#[test]
pub(super) fn deadline_expiry_precedes_matching_acknowledgement() {
    let mut harness = Harness::launch("delay-quiesce");
    harness.replace("normal", 2);
    harness.until(|master| master.state() == MasterState::Quiescing);
    let expired = harness.master.next_deadline().unwrap();
    thread::sleep(Duration::from_millis(100));
    let mut events = harness.master.poll(expired).unwrap();
    assert_ne!(harness.master.state(), MasterState::ActivatingCandidate);
    events.extend(harness.until(|master| master.state() == MasterState::Running));
    assert_eq!(harness.master.active_instance().unwrap().as_str(), "a");
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::StaleAcknowledgement {
            phase: ControlPhase::Quiesce,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MasterEvent::ReplacementCommitted { .. }))
    );
    assert_served_by(&harness, 1);
    harness.shutdown();
}

#[test]
pub(super) fn candidate_activation_ack_precedes_collected_old_active_exit() {
    let mut harness = Harness::launch("crash-after-quiesce");
    harness.replace("normal", 2);
    let mut events = harness.until(|master| master.state() == MasterState::ActivatingCandidate);
    thread::sleep(Duration::from_millis(100));
    events.extend(harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "b")
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ReplacementCommitted { active, .. } if active.as_str() == "b"
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MasterEvent::FailClosed { .. }))
    );
    assert_served_by(&harness, 2);
    harness.shutdown();
}

#[test]
pub(super) fn successful_replacement_has_exact_public_state_and_event_sequence() {
    let mut harness = Harness::launch("normal");
    harness.replace("normal", 2);
    let mut states = vec![harness.master.state()];
    let mut events = Vec::new();
    while harness.master.state() != MasterState::Running
        || harness.master.active_instance().unwrap().as_str() != "b"
    {
        events.extend(harness.master.poll(Instant::now()).unwrap());
        let state = harness.master.state();
        if states.last() != Some(&state) {
            states.push(state);
        }
        thread::sleep(POLL_INTERVAL);
    }

    assert_eq!(
        states,
        [
            MasterState::AdoptingCandidate,
            MasterState::Quiescing,
            MasterState::ActivatingCandidate,
            MasterState::DrainingRetired,
            MasterState::StoppingRetired,
            MasterState::Running,
        ]
    );
    assert_eq!(
        replacement_event_sequence(&events),
        [
            "started",
            "committed",
            "retired-termination-requested",
            "retired-exited",
            "completed",
        ]
    );
    harness.shutdown();
}

#[test]
pub(super) fn rollback_completes_after_reactivation_and_reap_in_either_order() {
    let mut reactivated_first = Harness::launch("normal");
    reactivated_first.replace("reject-activate", 2);
    let mut events = reactivated_first.until(|master| master.state() == MasterState::RollingBack);
    thread::sleep(Duration::from_millis(50));
    events.extend(reactivated_first.until(|master| master.state() == MasterState::Running));
    assert_eq!(
        rollback_event_sequence(&events),
        [
            "started",
            "rollback-started",
            "candidate-termination-requested",
            "candidate-exited",
            "rollback-completed",
        ]
    );
    reactivated_first.shutdown();

    let mut reaped_first = Harness::launch("delay-reactivate");
    reaped_first.replace("reject-activate", 2);
    let mut events = reaped_first.until(|master| {
        master.state() == MasterState::RollingBack
            && master.worker_state(WorkerRole::Candidate).is_none()
    });
    assert_eq!(reaped_first.master.state(), MasterState::RollingBack);
    events.extend(reaped_first.until(|master| master.state() == MasterState::Running));
    assert_eq!(
        rollback_event_sequence(&events),
        [
            "started",
            "rollback-started",
            "candidate-termination-requested",
            "candidate-exited",
            "rollback-completed",
        ]
    );
    reaped_first.shutdown();
}

#[test]
pub(super) fn deferred_old_active_exit_precedes_candidate_timeout_and_fails_closed() {
    let mut harness = Harness::launch("crash-after-quiesce");
    harness.replace("timeout-activate", 2);
    harness.until(|master| master.state() == MasterState::ActivatingCandidate);
    thread::sleep(Duration::from_millis(75));
    let mut events = harness.master.poll(Instant::now()).unwrap();
    assert_eq!(harness.master.state(), MasterState::ActivatingCandidate);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerExited {
            role: WorkerRole::Active,
            ..
        }
    )));

    let deadline = harness.master.next_deadline().unwrap();
    events.extend(harness.master.poll(deadline + POLL_INTERVAL).unwrap());
    events.extend(harness.until(|master| master.state() == MasterState::Failed));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::FailClosed {
            phase: FailurePhase::Crash
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MasterEvent::RollbackStarted { .. }))
    );
}

#[test]
pub(super) fn shutdown_completes_from_every_replacement_phase() {
    let mut adopting = Harness::launch("normal");
    adopting.replace("delay-adopt", 2);
    assert_shutdown_from_phase(adopting, MasterState::AdoptingCandidate);

    let mut quiescing = Harness::launch("delay-quiesce");
    quiescing.replace("normal", 2);
    quiescing.until(|master| master.state() == MasterState::Quiescing);
    assert_shutdown_from_phase(quiescing, MasterState::Quiescing);

    let mut activating = Harness::launch("normal");
    activating.replace("delay-activate", 2);
    activating.until(|master| master.state() == MasterState::ActivatingCandidate);
    assert_shutdown_from_phase(activating, MasterState::ActivatingCandidate);

    let mut rolling_back = Harness::launch("delay-reactivate");
    rolling_back.replace("reject-activate", 2);
    rolling_back.until(|master| master.state() == MasterState::RollingBack);
    assert_shutdown_from_phase(rolling_back, MasterState::RollingBack);

    let mut draining = Harness::launch("delay-drain");
    draining.replace("normal", 2);
    draining.until(|master| master.state() == MasterState::DrainingRetired);
    assert_shutdown_from_phase(draining, MasterState::DrainingRetired);

    let mut stopping = Harness::launch("delay-exit-after-shutdown");
    stopping.replace("normal", 2);
    stopping.until(|master| master.state() == MasterState::StoppingRetired);
    assert_shutdown_from_phase(stopping, MasterState::StoppingRetired);
}

#[test]
pub(super) fn retired_shutdown_ack_keeps_exit_deadline_until_delayed_reap() {
    let mut harness = Harness::launch("delay-exit-after-shutdown");
    harness.replace("normal", 2);
    harness.until(|master| master.state() == MasterState::StoppingRetired);
    let deadline = harness.master.next_deadline().unwrap();
    harness.until(|master| {
        master
            .worker_status(WorkerRole::Retired)
            .is_some_and(|status| {
                status.lifecycle == oxiroute_supervisor_master::WorkerLifecycle::Stopping
            })
    });
    assert_eq!(harness.master.next_deadline(), Some(deadline));
    assert_eq!(
        harness.master.worker_state(WorkerRole::Retired),
        Some(WorkerState::Terminating { forced: false })
    );

    let mut events = harness.master.poll(deadline + POLL_INTERVAL).unwrap();
    assert_eq!(
        harness.master.worker_state(WorkerRole::Retired),
        Some(WorkerState::Terminating { forced: true })
    );
    events.extend(harness.until(|master| master.state() == MasterState::Running));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RetiredFailed {
            phase: FailurePhase::Shutdown
        }
    )));
    harness.shutdown();
}

#[test]
pub(super) fn batched_old_active_status_follows_role_move_to_retired() {
    let mut harness = Harness::launch("delay-status-after-quiesce");
    harness.replace("normal", 2);
    while harness.master.state() != MasterState::ActivatingCandidate {
        harness.master.poll(Instant::now()).unwrap();
        thread::sleep(POLL_INTERVAL);
    }
    thread::sleep(Duration::from_millis(175));
    let events = harness.master.poll(Instant::now()).unwrap();
    let committed = events
        .iter()
        .position(|event| matches!(event, MasterEvent::ReplacementCommitted { .. }))
        .expect("candidate activation committed");
    let moved_status = events
        .iter()
        .position(|event| {
            matches!(
                event,
                MasterEvent::WorkerStatusUpdated {
                    role: WorkerRole::Retired,
                    instance_id,
                    ..
                } if instance_id.as_str() == "a"
            )
        })
        .expect("old active status resolved after the role move");
    assert!(committed < moved_status);
    harness.until(|master| master.state() == MasterState::Running);
    harness.shutdown();
}

#[test]
pub(super) fn replacement_supervisor_is_never_reconstructed() {
    let source = [
        include_str!("../../src/master.rs"),
        include_str!("../../src/master/dispatch.rs"),
        include_str!("../../src/master/executor.rs"),
        include_str!("../../src/master/failure.rs"),
        include_str!("../../src/master/model.rs"),
        include_str!("../../src/master/observation.rs"),
        include_str!("../../src/master/query.rs"),
        include_str!("../../src/master/replacement.rs"),
        include_str!("../../src/master/runtime.rs"),
    ]
    .concat();
    assert_eq!(source.matches("ReplacementSupervisor::new(").count(), 1);
    assert!(!source.contains("expect_action"));
}

#[test]
pub(super) fn channel_disconnect_waits_for_exit_and_rolls_back_consistently() {
    let mut harness = Harness::launch("normal");
    harness.replace("disconnect-activate", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "a")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerDisconnected {
            role: WorkerRole::Candidate,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RollbackStarted {
            phase: FailurePhase::Disconnected,
            ..
        }
    )));
    assert_served_by(&harness, 1);
    harness.shutdown();
}
