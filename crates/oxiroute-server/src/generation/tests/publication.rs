#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn candidate_can_be_superseded_while_the_gate_is_quiescing() {
    let manager = GenerationManager::new();
    let first = manager
        .prepare(document_with_max_connections(Some(1)))
        .expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let mut acceptor = first.generation().accept_gate().register();
    let ownership = acceptor.claim().expect("accept ownership");
    let activation_manager = manager.clone();
    let activation_candidate = second.clone();
    let (activated_tx, activated_rx) = mpsc::sync_channel(1);
    let activation = thread::spawn(move || {
        activated_tx
            .send(
                activation_manager
                    .activate(&activation_candidate)
                    .map(|_| ())
                    .map_err(|error| error.code()),
            )
            .expect("activation receiver");
    });

    let gate_state = acceptor.changed().await.expect("gate close");
    let third = manager
        .prepare(document_with_max_connections(Some(3)))
        .expect("third candidate");
    assert!(candidate_matches(manager.candidate().as_ref(), &third));
    assert!(matches!(
        manager.activate(&third),
        Err(GenerationError::MutationInProgress)
    ));
    acceptor.acknowledge(gate_state.epoch);
    drop(ownership);

    assert_eq!(
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result"),
        Err(GenerationError::CandidateSuperseded.code())
    );
    activation.join().expect("activation thread");
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(first.generation().accepting());
    assert!(!second.generation().accepting());
    assert!(candidate_matches(manager.candidate().as_ref(), &third));
    assert_eq!(manager.status().failures, 0);
    drop(acceptor);
    let active = manager.activate(&third).expect("third activation");
    assert!(Arc::ptr_eq(&active, third.generation()));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_activation_is_rejected_without_touching_the_gate() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let mut acceptor = first.generation().accept_gate().register();
    let ownership = acceptor.claim().expect("accept ownership");
    let activation_manager = manager.clone();
    let activation_candidate = second.clone();
    let (activated_tx, activated_rx) = mpsc::sync_channel(1);
    let activation = thread::spawn(move || {
        activated_tx
            .send(
                activation_manager
                    .activate(&activation_candidate)
                    .map(|_| ())
                    .map_err(|error| error.code()),
            )
            .expect("activation receiver");
    });

    let gate_state = acceptor.changed().await.expect("gate close");
    assert!(matches!(
        manager.activate(&second),
        Err(GenerationError::MutationInProgress)
    ));
    assert!(activated_rx.try_recv().is_err());
    acceptor.acknowledge(gate_state.epoch);
    drop(ownership);
    activated_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("activation result")
        .expect("activation");
    activation.join().expect("activation thread");
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        second.generation()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn quarantine_during_publication_cancels_once_and_reopens_the_active_gate() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let mut startup = manager
        .begin_candidate_start(&second)
        .expect("startup reservation");
    claim_and_mark_runtime_started(&mut startup);
    let mut acceptor = first.generation().accept_gate().register();
    let ownership = acceptor.claim().expect("accept ownership");
    let (activated_tx, activated_rx) = mpsc::sync_channel(1);
    let activation = thread::spawn(move || {
        activated_tx
            .send(startup.activate().map(|_| ()).map_err(|error| error.code()))
            .expect("activation receiver");
    });

    let gate_state = acceptor.changed().await.expect("gate close");
    manager.quarantine(&second, "runtime_start");
    manager.quarantine(&second, "runtime_start");
    assert_eq!(manager.status().failures, 1);
    acceptor.acknowledge(gate_state.epoch);
    drop(ownership);

    assert_eq!(
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result"),
        Err(GenerationError::CandidateSuperseded.code())
    );
    activation.join().expect("activation thread");
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(first.generation().accepting());
    assert!(manager.candidate().is_none());
    assert_eq!(manager.status().failures, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_failure_during_publication_is_quarantined_without_retry() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let mut startup = manager
        .begin_candidate_start(&second)
        .expect("startup reservation");
    claim_and_mark_runtime_started(&mut startup);
    let mut acceptor = first.generation().accept_gate().register();
    let ownership = acceptor.claim().expect("accept ownership");
    let (activated_tx, activated_rx) = mpsc::sync_channel(1);
    let activation = thread::spawn(move || {
        activated_tx
            .send(startup.activate().map(|_| ()).map_err(|error| error.code()))
            .expect("activation receiver");
    });

    let gate_state = acceptor.changed().await.expect("gate close");
    second.generation().mark_runtime_failed();
    acceptor.acknowledge(gate_state.epoch);
    drop(ownership);

    assert_eq!(
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result"),
        Err(GenerationError::RuntimePrepare.code())
    );
    activation.join().expect("activation thread");
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(first.generation().accepting());
    assert!(manager.candidate().is_none());
    assert_eq!(manager.status().failures, 1);
    assert!(matches!(
        manager.begin_candidate_start(&second),
        Err(GenerationError::CandidateSuperseded)
    ));
}

#[test]
fn supersession_precedes_runtime_failure_and_quiesce_timeout() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let reservation = manager
        .reserve_activation(&second, None)
        .expect("activation reservation");
    let close = first.generation().accept_gate().close();
    let third = manager
        .prepare(document_with_max_connections(Some(3)))
        .expect("third candidate");
    second.generation().mark_runtime_failed();

    assert!(matches!(
        manager.publish_reserved_candidate(&second, &reservation, Some(close), false),
        Err(GenerationError::CandidateSuperseded)
    ));
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(first.generation().accepting());
    assert!(candidate_matches(manager.candidate().as_ref(), &third));
    assert_eq!(manager.status().failures, 0);
}

#[test]
fn runtime_failure_precedes_quiesce_timeout_and_uses_runtime_start_quarantine() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let mut startup = manager
        .begin_candidate_start(&second)
        .expect("startup reservation");
    claim_and_mark_runtime_started(&mut startup);
    let reservation = manager
        .reserve_activation(&second, Some(startup.reservation_token))
        .expect("activation reservation");
    let close = first.generation().accept_gate().close();
    second.generation().mark_runtime_failed();

    assert!(matches!(
        manager.publish_reserved_candidate(&second, &reservation, Some(close), false),
        Err(GenerationError::RuntimePrepare)
    ));
    startup.completed = true;
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(first.generation().accepting());
    assert!(manager.candidate().is_none());
    let status = manager.status();
    assert_eq!(status.failures, 1);
    assert_eq!(status.last_failure, Some("runtime_start"));
}

#[test]
fn gate_timeout_consumes_a_started_candidate_and_reopens_the_active_generation() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let ownership = first
        .generation()
        .begin_admission()
        .expect("accept ownership");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let mut startup = manager
        .begin_candidate_start(&second)
        .expect("startup reservation");
    claim_and_mark_runtime_started(&mut startup);

    assert!(matches!(
        startup.activate_with_timeout(Duration::ZERO),
        Err(GenerationError::AcceptorQuiesce)
    ));
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(first.generation().accepting());
    assert!(manager.candidate().is_none());
    let status = manager.status();
    assert_eq!(status.failures, 1);
    assert_eq!(
        status.quarantined_revision,
        Some(second.revision().candidate.clone())
    );
    assert!(matches!(
        manager.begin_candidate_start(&second),
        Err(GenerationError::CandidateSuperseded)
    ));
    drop(ownership);
}

#[test]
fn gate_timeout_keeps_an_unstarted_candidate_retryable() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let ownership = first
        .generation()
        .begin_admission()
        .expect("accept ownership");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");

    assert!(matches!(
        manager.activate_inner(&second, None, Duration::ZERO),
        Err(GenerationError::AcceptorQuiesce)
    ));
    assert!(candidate_matches(manager.candidate().as_ref(), &second));
    assert!(first.generation().accepting());
    assert_eq!(manager.status().failures, 0);
    drop(ownership);

    let active = manager
        .activate(&second)
        .expect("retry unstarted candidate");
    assert!(Arc::ptr_eq(&active, second.generation()));
}

#[tokio::test(flavor = "current_thread")]
async fn failed_publication_does_not_reopen_an_already_closed_active_gate() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let mut acceptor = first.generation().accept_gate().register();
    first.generation().stop_accepting();
    assert!(!first.generation().accepting());
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");

    assert!(matches!(
        manager.activate_inner(&second, None, Duration::ZERO),
        Err(GenerationError::AcceptorQuiesce)
    ));
    assert!(!first.generation().accepting());
    assert!(candidate_matches(manager.candidate().as_ref(), &second));
    let gate_state = acceptor.changed().await.expect("gate close");
    acceptor.acknowledge(gate_state.epoch);
}

#[test]
fn publication_failure_does_not_undo_a_later_shutdown_close() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let reservation = manager
        .reserve_activation(&second, None)
        .expect("activation reservation");
    let activation_close = first.generation().accept_gate().close();
    first.generation().stop_accepting();

    assert!(matches!(
        manager.publish_reserved_candidate(&second, &reservation, Some(activation_close), false,),
        Err(GenerationError::AcceptorQuiesce)
    ));
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(!first.generation().accepting());
    assert!(candidate_matches(manager.candidate().as_ref(), &second));
}
