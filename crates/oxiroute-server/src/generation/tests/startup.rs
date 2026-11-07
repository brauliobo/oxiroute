#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn cancelling_a_claimed_start_rejects_a_late_marker_and_consumes_the_candidate_once() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("candidate");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("startup reservation");
    let generation = startup.claim_runtime_start().expect("runtime start claim");
    assert!(matches!(
        startup.claim_runtime_start(),
        Err(GenerationError::CandidateSuperseded)
    ));
    let marker_generation = Arc::clone(&generation);
    let (marker_ready, start_marker) = mpsc::sync_channel(0);
    let marker = thread::spawn(move || {
        start_marker.recv().expect("marker release");
        marker_generation.mark_runtime_started()
    });

    drop(startup);
    marker_ready.send(()).expect("marker receiver");

    assert!(!marker.join().expect("runtime marker"));
    assert!(generation.runtime_failed());
    assert!(!generation.claim_runtime_start());
    assert!(manager.candidate().is_none());
    assert_eq!(manager.status().failures, 1);
    manager.quarantine(&candidate, "runtime_start");
    assert_eq!(manager.status().failures, 1);
    assert!(matches!(
        manager.begin_candidate_start(&candidate),
        Err(GenerationError::CandidateSuperseded)
    ));
}

#[test]
fn activation_before_runtime_claim_releases_only_the_unstarted_reservation() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("candidate");
    let startup = manager
        .begin_candidate_start(&candidate)
        .expect("startup reservation");

    assert!(matches!(
        startup.activate(),
        Err(GenerationError::RuntimePrepare)
    ));
    assert!(candidate_matches(manager.candidate().as_ref(), &candidate));
    assert!(!candidate.generation().runtime_failed());
    assert_eq!(manager.status().failures, 0);

    let mut next_startup = manager
        .begin_candidate_start(&candidate)
        .expect("next startup reservation");
    claim_and_mark_runtime_started(&mut next_startup);
    let active = next_startup.activate().expect("activation");
    assert!(Arc::ptr_eq(&active, candidate.generation()));
}

#[test]
fn stale_startup_token_cannot_cancel_a_new_reservation() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    let active = manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let stale = manager
        .begin_candidate_start(&second)
        .expect("first startup reservation");
    let stale_token = stale.reservation_token;
    drop(stale);
    let mut current = manager
        .begin_candidate_start(&second)
        .expect("second startup reservation");

    manager.cancel_startup(&second, stale_token);
    assert!(matches!(
        manager.begin_mutation(&active.revision().candidate),
        Err(GenerationError::MutationInProgress)
    ));
    claim_and_mark_runtime_started(&mut current);
    let activated = current.activate().expect("current startup activation");
    assert!(Arc::ptr_eq(&activated, second.generation()));
}

#[test]
fn quarantined_candidate_leaves_active_unchanged_and_can_be_prepared_again() {
    let manager = GenerationManager::new();
    let active_candidate = manager.prepare(document()).expect("active candidate");
    let active = manager.activate(&active_candidate).expect("active");
    let failed = manager.prepare(document()).expect("failed candidate");

    manager.quarantine(&failed, "runtime_start");
    assert!(Arc::ptr_eq(
        &manager.active().expect("active retained"),
        &active
    ));
    assert!(active.accepting());
    assert!(manager.status().quarantined_revision.is_some());
    assert!(manager.prepare(document()).is_ok());
}

#[test]
fn failed_rollback_revision_cannot_be_started_again() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let mut second_config = first.generation().config.to_draft();
    second_config.max_connections = Some(2);
    let second = manager
        .prepare(document_for(&second_config))
        .expect("second candidate");
    manager.activate(&second).expect("second activation");

    let rollback = manager.rollback().expect("rollback candidate");
    manager.quarantine(&rollback, "runtime_start");
    assert!(matches!(
        manager.rollback(),
        Err(GenerationError::QuarantinedRevision)
    ));
    assert!(Arc::ptr_eq(
        &manager.active().expect("second remains active"),
        second.generation()
    ));
}

#[test]
fn stale_revision_cannot_acquire_a_mutation_permit() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("candidate");
    let active = manager.activate(&candidate).expect("active");
    let stale = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        .parse()
        .unwrap();

    assert!(matches!(
        manager.begin_mutation(&stale),
        Err(GenerationError::RevisionConflict)
    ));
    assert!(manager.begin_mutation(&active.revision().candidate).is_ok());
}

#[test]
fn candidate_start_waits_for_mutation_and_then_excludes_new_mutations_until_publication() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    let active = manager.activate(&first).expect("active");
    let second = manager.prepare(document()).expect("second candidate");
    let mutation = manager
        .begin_mutation(&active.revision().candidate)
        .expect("active mutation");

    assert!(matches!(
        manager.begin_candidate_start(&second),
        Err(GenerationError::MutationInProgress)
    ));
    drop(mutation);

    let mut startup = manager
        .begin_candidate_start(&second)
        .expect("reserved candidate startup");
    assert!(matches!(
        manager.begin_mutation(&active.revision().candidate),
        Err(GenerationError::MutationInProgress)
    ));
    claim_and_mark_runtime_started(&mut startup);
    let activated = startup.activate().expect("reserved activation");
    assert!(Arc::ptr_eq(&activated, second.generation()));
}
