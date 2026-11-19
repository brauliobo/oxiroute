#[allow(clippy::wildcard_imports)]
use super::*;

fn active_is(name: &'static str) -> impl Fn(&Master<SystemActionExecutor>) -> bool {
    move |master| {
        master
            .active_instance()
            .is_some_and(|instance| instance.as_str() == name)
    }
}

fn draining(master: &Master<SystemActionExecutor>) -> Vec<String> {
    master
        .retired_workers()
        .iter()
        .map(|worker| worker.instance_id.as_str().to_owned())
        .collect()
}

#[test]
pub(super) fn a_replacement_starts_while_the_previous_worker_still_drains() {
    let mut harness = Harness::launch("normal");
    let hold_a = harness.hold(1, ControlPhase::Drain);
    harness.replace_as("b", "normal", 2);
    harness.until(active_is("b"));
    assert_eq!(harness.master.state(), MasterState::DrainingRetired);
    assert!(harness.master.accepts_replacement());

    let hold_b = harness.hold(2, ControlPhase::Drain);
    harness.replace_as("c", "normal", 3);
    harness.until(active_is("c"));
    assert_eq!(draining(&harness.master), ["a", "b"], "oldest first");
    assert_served_by(&harness, 3);

    drop(hold_a);
    harness.until(|master| draining(master) == ["b"]);
    drop(hold_b);
    harness.until(|master| draining(master).is_empty() && master.state() == MasterState::Running);
    assert_served_by(&harness, 3);
    harness.shutdown();
}

#[test]
pub(super) fn a_replacement_beyond_the_limit_closes_the_oldest_draining_worker() {
    let mut harness = Harness::launch("normal");
    harness.master.set_retired_limit(2);
    let hold_a = harness.hold(1, ControlPhase::Drain);
    let hold_b = harness.hold(2, ControlPhase::Drain);
    let hold_c = harness.hold(3, ControlPhase::Drain);
    harness.replace_as("b", "normal", 2);
    harness.until(active_is("b"));
    harness.replace_as("c", "normal", 3);
    harness.until(active_is("c"));
    assert_eq!(draining(&harness.master), ["a", "b"]);
    assert!(
        harness
            .master
            .retired_workers()
            .iter()
            .all(|worker| worker.forced.is_none())
    );

    harness.replace_as("d", "normal", 4);
    let events = harness.until(active_is("d"));
    assert!(
        events.iter().any(|event| matches!(
            event,
            MasterEvent::RetiredForced { instance_id, reason: RetiredForce::RetiredLimit }
                if instance_id.as_str() == "a"
        )),
        "{events:?}"
    );
    let retired = harness.master.retired_workers();
    assert_eq!(retired.len(), 3);
    assert_eq!(retired[0].forced, Some(RetiredForce::RetiredLimit));
    assert!(retired[1..].iter().all(|worker| worker.forced.is_none()));

    drop(hold_a);
    let events = harness.until(|master| draining(master) == ["b", "c"]);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerExited { role: WorkerRole::Retired, instance_id, .. }
            if instance_id.as_str() == "a"
    )));
    drop((hold_b, hold_c));
    harness.until(|master| draining(master).is_empty());
    harness.shutdown();
}

#[test]
pub(super) fn a_detached_worker_past_its_drain_deadline_is_killed_and_reaped() {
    let mut harness = Harness::launch("normal");
    let hold_a = harness.hold(1, ControlPhase::Drain);
    let stuck = harness.master.worker_id(WorkerRole::Active).unwrap();
    harness.replace_as("b", "normal", 2);
    harness.until(active_is("b"));
    harness.replace_as("c", "normal", 3);
    harness.until(active_is("c"));
    assert!(draining(&harness.master).contains(&"a".to_owned()));

    let events = harness.expire_deadline();
    let events = events
        .into_iter()
        .chain(harness.until(|master| !draining(master).contains(&"a".to_owned())))
        .collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            MasterEvent::RetiredFailed {
                phase: FailurePhase::Drain
            }
        )),
        "{events:?}"
    );
    wait_process_gone(stuck);
    assert_served_by(&harness, 3);
    drop(hold_a);
    harness.shutdown();
}

#[test]
pub(super) fn shutting_down_reaps_every_draining_worker() {
    let mut harness = Harness::launch("normal");
    let hold_a = harness.hold(1, ControlPhase::Drain);
    let first = harness.master.worker_id(WorkerRole::Active).unwrap();
    harness.replace_as("b", "normal", 2);
    harness.until(active_is("b"));
    let hold_b = harness.hold(2, ControlPhase::Drain);
    let second = harness.master.worker_id(WorkerRole::Active).unwrap();
    harness.replace_as("c", "normal", 3);
    harness.until(active_is("c"));
    assert_eq!(draining(&harness.master), ["a", "b"]);

    assert!(matches!(
        harness.master.shutdown(harness.now).unwrap(),
        ShutdownProgress::Pending { remaining: 3, .. }
    ));
    let mut events = harness.expire_deadline();
    events.extend(harness.until(|master| master.state() == MasterState::Stopped));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
    wait_process_gone(first);
    wait_process_gone(second);
    drop((hold_a, hold_b));
}
