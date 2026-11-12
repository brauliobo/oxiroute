#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn replaces_a_with_b_using_the_same_tcp_and_unix_listener_identities() {
    let mut harness = Harness::launch("normal");
    let manifest = harness.master.listener_manifest().clone();
    harness.replace("normal", 2);
    assert_eq!(harness.master.state(), MasterState::AdoptingCandidate);
    assert_served_by(&harness, 1);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "b")
    });
    assert_eq!(harness.master.listener_manifest(), &manifest);
    assert_served_by(&harness, 2);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ReplacementCommitted { active, retired }
            if active.as_str() == "b" && retired.as_str() == "a"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ReplacementCompleted { active } if active.as_str() == "b"
    )));
    let events = harness.shutdown();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
}

#[test]
pub(super) fn a_replacement_activates_with_the_state_its_predecessor_reported_and_notices_reach_the_active()
 {
    let mut harness = Harness::launch("echo-state");
    harness.replace("echo-state", 2);
    harness.until(committed_as_b);
    harness.until(|master| {
        master
            .worker_status(WorkerRole::Active)
            .is_some_and(|status| {
                status.degradation.as_deref() == Some("state-of-the-quiesced-worker|")
            })
    });

    harness.master.notify_active(b"rejected-file").unwrap();
    harness.until(|master| {
        master
            .worker_status(WorkerRole::Active)
            .is_some_and(|status| {
                status.degradation.as_deref() == Some("state-of-the-quiesced-worker|rejected-file")
            })
    });
    harness.shutdown();
}

pub(super) fn committed_as_b(master: &Master<SystemActionExecutor>) -> bool {
    master.state() == MasterState::Running
        && master
            .active_instance()
            .is_some_and(|id| id.as_str() == "b")
}

#[test]
pub(super) fn a_replacement_that_adds_a_listener_serves_it_once_committed() {
    let mut harness = Harness::launch("normal");
    let added = TcpListener::bind("127.0.0.1:0").unwrap();
    let added_address = added.local_addr().unwrap();
    harness.replace_with_listeners(
        "normal",
        2,
        NextSet {
            keep_tcp: true,
            keep_unix: true,
            add_tcp: Some(added),
        },
    );
    assert_eq!(harness.master.listener_manifest().slots().len(), 2);
    harness.until(committed_as_b);
    assert_eq!(harness.master.listener_manifest().slots().len(), 3);
    assert_served_by(&harness, 2);
    assert_eq!(probe_tcp(added_address), 2);
    harness.shutdown();
}

#[test]
pub(super) fn a_replacement_that_removes_a_tcp_listener_refuses_new_connections_and_keeps_the_rest()
{
    let mut harness = Harness::launch("normal");
    harness.replace_with_listeners(
        "normal",
        2,
        NextSet {
            keep_tcp: false,
            keep_unix: true,
            add_tcp: None,
        },
    );
    harness.until(committed_as_b);
    assert_eq!(harness.master.listener_manifest().slots().len(), 1);
    // The test and the retired worker still hold descriptors of the removed socket; it stops
    // listening anyway because the shutdown applies to the socket, not to one descriptor.
    assert!(TcpStream::connect(harness.tcp_address).is_err());
    assert_eq!(probe_unix(&harness.unix_path), 2);
    harness.shutdown();
}

#[test]
pub(super) fn a_replacement_that_removes_a_unix_listener_refuses_new_connections_and_keeps_the_rest()
 {
    let mut harness = Harness::launch("normal");
    harness.replace_with_listeners(
        "normal",
        2,
        NextSet {
            keep_tcp: true,
            keep_unix: false,
            add_tcp: None,
        },
    );
    harness.until(committed_as_b);
    assert!(UnixStream::connect(&harness.unix_path).is_err());
    assert_eq!(probe_tcp(harness.tcp_address), 2);
    harness.shutdown();
}

#[test]
pub(super) fn a_replacement_that_changes_nothing_keeps_every_listener_serving() {
    let mut harness = Harness::launch("normal");
    harness.replace_with_listeners(
        "normal",
        2,
        NextSet {
            keep_tcp: true,
            keep_unix: true,
            add_tcp: None,
        },
    );
    harness.until(committed_as_b);
    assert_served_by(&harness, 2);
    harness.shutdown();
}

#[test]
pub(super) fn a_rolled_back_replacement_discards_the_listeners_it_added() {
    let mut harness = Harness::launch("normal");
    let manifest = harness.master.listener_manifest().clone();
    let added = TcpListener::bind("127.0.0.1:0").unwrap();
    let added_socket = descriptor_target(&added);
    harness.replace_with_listeners(
        "reject-activate",
        2,
        NextSet {
            keep_tcp: true,
            keep_unix: true,
            add_tcp: Some(added),
        },
    );
    let events = harness.until(|master| master.state() == MasterState::Running);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::RollbackCompleted { .. })),
        "events: {events:?}"
    );
    assert_eq!(harness.master.listener_manifest(), &manifest);
    assert_served_by(&harness, 1);
    // The candidate that adopted the added socket is gone, and the master dropped its own copy.
    let started = Instant::now();
    while socket_is_open_in_any_process(&added_socket) {
        harness.master.poll(Instant::now()).unwrap();
        assert!(
            started.elapsed() < TEST_TIMEOUT,
            "the added listener outlived the rolled back replacement"
        );
        thread::sleep(Duration::from_millis(20));
    }
    harness.shutdown();
}

#[test]
pub(super) fn a_replacement_that_fails_to_spawn_discards_the_listeners_it_brought() {
    let mut harness = Harness::launch("normal");
    let added = TcpListener::bind("127.0.0.1:0").unwrap();
    let added_socket = descriptor_target(&added);
    let listeners = harness.kept.next_set(
        harness.tcp_address,
        &harness.unix_path,
        NextSet {
            keep_tcp: true,
            keep_unix: true,
            add_tcp: Some(added),
        },
    );
    let mut failing = FailingFactory::default();
    let result = harness.master.replace_with_listeners(
        &mut failing,
        worker("b", 2, "normal"),
        listeners,
        Instant::now(),
    );
    assert!(result.is_err() || failing.calls == 1);
    harness.until(|master| master.state() == MasterState::Running);
    assert_eq!(harness.master.listener_manifest().slots().len(), 2);
    assert!(!socket_is_open_in_any_process(&added_socket));
    assert_served_by(&harness, 1);
    harness.shutdown();
}

#[test]
pub(super) fn the_drain_window_may_last_a_day_while_other_phases_stay_within_an_hour() {
    let second = Duration::from_secs(1);
    let config = |drain, shutdown| MasterConfig::new(second, second, second, drain, shutdown);
    assert!(config(Duration::from_hours(24), second).is_ok());
    assert!(config(Duration::from_hours(24) + second, second).is_err());
    assert!(config(second, Duration::from_hours(1)).is_ok());
    assert!(config(second, Duration::from_hours(1) + second).is_err());
    assert!(config(Duration::ZERO, second).is_err());
}

#[test]
pub(super) fn authenticated_worker_status_is_retained_without_creating_a_command_path() {
    let harness = Harness::launch("normal");
    let status = harness
        .master
        .worker_status(WorkerRole::Active)
        .expect("active worker status");
    assert_eq!(status.sequence, 2);
    assert_eq!(status.generation_id, GenerationId(1));
    assert_eq!(
        status.lifecycle,
        oxiroute_supervisor_master::WorkerLifecycle::Active
    );
    assert!(status.runtime_started);
    assert!(!status.degraded);
    let events = harness.master.worker_events(0, 64);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].generation_id, GenerationId(1));
    assert_eq!(events[0].worker_event.cursor, 1);
    let catalog = SupervisedGenerationCatalog::new(GenerationLaunchDocument::new(
        InstanceId::new("a").unwrap(),
        GenerationId(1),
        Revision(10),
        (),
    ));
    let snapshot = harness.master.supervisor_snapshot(&catalog).unwrap();
    assert_eq!(snapshot.generations.len(), 1);
    assert_eq!(snapshot.generations[0].role, GenerationRole::Active);
    assert_eq!(snapshot.generations[0].revision, Revision(10));
    assert_eq!(snapshot.processes.len(), 1);
    assert_eq!(snapshot.processes[0].role, GenerationRole::Active);
    assert_eq!(snapshot.processes[0].lifecycle, status.lifecycle);
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].role, GenerationRole::Active);
    drop(harness.shutdown());
}

#[test]
pub(super) fn supervisor_snapshot_rejects_a_catalog_for_a_different_process_identity() {
    let harness = Harness::launch("normal");
    let catalog = SupervisedGenerationCatalog::new(GenerationLaunchDocument::new(
        InstanceId::new("other").unwrap(),
        GenerationId(1),
        Revision(10),
        (),
    ));

    let error = harness.master.supervisor_snapshot(&catalog).unwrap_err();

    assert_eq!(error.instance_id.as_str(), "a");
    assert_eq!(error.observed_role, GenerationRole::Active);
    assert_eq!(error.catalog_role, None);
    drop(harness.shutdown());
}

#[test]
pub(super) fn malformed_worker_status_fails_closed_after_replacement_commit() {
    let mut harness = Harness::launch("normal");
    harness.replace("malformed-status-after-activate", 2);
    let events = harness.until(|master| master.state() == MasterState::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::FailClosed {
            phase: FailurePhase::Protocol
        }
    )));
}

#[test]
pub(super) fn initial_adoption_and_activation_fail_closed() {
    for (behavior, expected) in [
        ("reject-adopt", FailurePhase::ListenerAdoption),
        ("reject-activate", FailurePhase::Activation),
    ] {
        let (_temporary, listeners, _, _, _, _) = listeners();
        let mut factory = factory();
        let mut master = Master::launch(
            config(),
            listeners,
            &mut factory,
            worker("a", 1, behavior),
            Instant::now(),
        )
        .unwrap();
        let started = Instant::now();
        let mut events = Vec::new();
        while master.state() != MasterState::Failed {
            events.extend(master.poll(Instant::now()).unwrap());
            assert!(started.elapsed() < TEST_TIMEOUT);
            thread::sleep(POLL_INTERVAL);
        }
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::Failed { phase } if *phase == expected
        )));
    }
}

#[test]
pub(super) fn rolls_back_candidate_rejections_during_adoption_and_activation() {
    for behavior in ["reject-adopt", "reject-activate"] {
        let mut harness = Harness::launch("normal");
        harness.replace(behavior, 2);
        let events = harness.until(|master| {
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|id| id.as_str() == "a")
        });
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::RollbackStarted { candidate, .. } if candidate.as_str() == "b"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::RollbackCompleted { active } if active.as_str() == "a"
        )));
        harness.shutdown();
    }
}

#[test]
pub(super) fn rolls_back_when_active_rejects_quiesce() {
    let mut harness = Harness::launch("reject-quiesce");
    harness.replace("normal", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "a")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RollbackStarted {
            phase: FailurePhase::Quiesce,
            ..
        }
    )));
    harness.shutdown();
}

#[test]
pub(super) fn retired_drain_failure_keeps_the_committed_candidate_active() {
    let mut harness = Harness::launch("reject-drain");
    harness.replace("normal", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "b")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RetiredFailed {
            phase: FailurePhase::Drain
        }
    )));
    harness.shutdown();
}

#[test]
pub(super) fn a_retiring_worker_that_outlasts_the_default_drain_window_is_failed() {
    let mut harness = Harness::launch("slow-drain");
    harness.replace("normal", 2);
    let events = harness.until(|master| master.state() == MasterState::Running);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RetiredFailed {
            phase: FailurePhase::Drain
        }
    )));
    harness.shutdown();
}

#[test]
pub(super) fn the_retirement_deadline_of_a_replacement_replaces_the_default_drain_window() {
    let mut harness = Harness::launch("slow-drain");
    harness
        .master
        .set_retirement_deadline(Duration::from_secs(2));
    harness.replace("normal", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "b")
    });
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MasterEvent::RetiredFailed { .. })),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ReplacementCompleted { .. }))
    );
    harness.shutdown();
}

#[test]
pub(super) fn failed_reactivation_fails_closed() {
    let mut harness = Harness::launch("reject-reactivate");
    harness.replace("reject-activate", 2);
    let events = harness.until(|master| master.state() == MasterState::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::Failed {
            phase: FailurePhase::Reactivation
        }
    )));
}

#[test]
pub(super) fn stale_acknowledgement_is_observed_without_advancing_the_phase() {
    let mut harness = Harness::launch("normal");
    harness.replace("stale-adopt", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "b")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::StaleAcknowledgement { request_id: 2, .. }
    )));
    harness.shutdown();
}

/// A probe that gives up on a connection leaves it for the worker to find: the worker must go on serving.
#[test]
pub(super) fn a_client_that_leaves_before_the_answer_does_not_stop_the_worker_serving() {
    let harness = Harness::launch("normal");
    for _ in 0..20 {
        drop(UnixStream::connect(&harness.unix_path).unwrap());
        drop(TcpStream::connect(harness.tcp_address).unwrap());
    }
    assert_served_by(&harness, 1);
    harness.shutdown();
}

/// The activation deadline is 300 ms: a test thread that stalls for longer than that before it polls the master
/// again, as on a loaded host, still sees the candidate's exit reported as the crash it was.
#[test]
pub(super) fn a_candidate_crash_is_a_crash_however_late_the_master_polls() {
    let mut harness = Harness::launch("normal");
    harness.replace("crash-activate", 2);
    let events = harness.candidate_crashes_on_activation(Duration::from_millis(600));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RollbackStarted {
            phase: FailurePhase::Crash,
            ..
        }
    )));
    harness.shutdown();
}

#[test]
pub(super) fn candidate_crash_rolls_back_and_active_crash_fails_closed() {
    let mut candidate = Harness::launch("normal");
    candidate.replace("crash-activate", 2);
    let mut events = candidate.candidate_crashes_on_activation(Duration::ZERO);
    events.extend(candidate.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "a")
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RollbackStarted {
            phase: FailurePhase::Crash,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerExited {
            role: WorkerRole::Candidate,
            ..
        }
    )));
    candidate.shutdown();

    let mut active = Harness::launch("crash-notice");
    active.crash_active();
    let events = active.until(|master| master.state() == MasterState::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::Failed {
            phase: FailurePhase::Crash
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerExited {
            role: WorkerRole::Active,
            ..
        }
    )));
}

#[test]
pub(super) fn failed_candidates_do_not_leak_parent_descriptors() {
    let mut harness = Harness::launch("normal");
    let baseline = matching_descriptor_count(&harness.listener_targets);
    // The master's two originals plus the two handles the harness keeps for later listener sets.
    assert_eq!(baseline, 4);
    for generation in 2..=5 {
        harness.replace("reject-adopt", generation);
        harness.until(|master| master.state() == MasterState::Running);
        assert_eq!(
            matching_descriptor_count(&harness.listener_targets),
            baseline
        );
    }
    harness.shutdown();
}
