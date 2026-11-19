#[allow(clippy::wildcard_imports)]
use super::*;

const WINDOW: Duration = Duration::from_mins(1);

#[test]
pub(super) fn a_handed_over_worker_drains_and_the_master_stops_with_it() {
    let mut harness = Harness::launch("normal");
    let hold = harness.hold(1, ControlPhase::Drain);
    harness
        .master
        .hand_over(WINDOW, Instant::now())
        .expect("hand over the active worker");
    assert_eq!(harness.master.state(), MasterState::HandedOver);
    assert!(harness.master.active_instance().is_none());
    assert!(!harness.master.accepts_replacement());
    let retired = harness.master.retired_workers();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].window, WINDOW);

    drop(hold);
    let events = harness.until(|master| master.state() == MasterState::Stopped);
    assert!(
        events.iter().any(|event| matches!(
            event,
            MasterEvent::WorkerExited {
                role: WorkerRole::Retired,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { forced: false }))
    );
}

#[test]
pub(super) fn a_handed_over_worker_past_its_deadline_is_killed_and_the_master_stops() {
    let mut harness = Harness::launch("normal");
    let hold = harness.hold(1, ControlPhase::Drain);
    let stuck = harness.master.worker_id(WorkerRole::Active).unwrap();
    harness
        .master
        .hand_over(WINDOW, Instant::now())
        .expect("hand over the active worker");

    harness.expire_deadline();
    harness.until(|master| master.state() == MasterState::Stopped);
    wait_process_gone(stuck);
    drop(hold);
}

#[test]
pub(super) fn a_master_stopped_while_handed_over_closes_its_draining_worker() {
    let mut harness = Harness::launch("normal");
    let hold = harness.hold(1, ControlPhase::Drain);
    harness
        .master
        .hand_over(WINDOW, Instant::now())
        .expect("hand over the active worker");
    assert!(matches!(
        harness.master.shutdown(harness.now).unwrap(),
        ShutdownProgress::Pending { .. }
    ));
    drop(hold);
    harness.until(|master| master.state() == MasterState::Stopped);
}

#[test]
pub(super) fn a_master_hands_over_only_while_it_runs_without_a_replacement() {
    let mut harness = Harness::launch("normal");
    let hold = harness.hold(1, ControlPhase::Drain);
    harness.replace_as("b", "normal", 2);
    assert!(matches!(
        harness.master.hand_over(WINDOW, Instant::now()),
        Err(MasterError::InvalidState(_))
    ));
    harness.until(|master| master.state() == MasterState::DrainingRetired);
    assert!(matches!(
        harness.master.hand_over(WINDOW, Instant::now()),
        Err(MasterError::InvalidState(MasterState::DrainingRetired))
    ));
    drop(hold);
    harness.until(|master| master.state() == MasterState::Running);
    harness.shutdown();
}

#[test]
pub(super) fn the_listeners_are_exported_for_a_successor() {
    let harness = Harness::launch("normal");
    let exported = harness.master.export_listeners().expect("duplicates");
    assert_eq!(
        exported.len(),
        harness.master.listener_manifest().slots().len()
    );
    drop(exported);
    harness.shutdown();
}
