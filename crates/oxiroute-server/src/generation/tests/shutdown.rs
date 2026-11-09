#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn shutdown_releases_manager_state_while_owned_generations_drain() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let mut acceptor = first.generation().accept_gate().register();
    let ownership = acceptor.claim().expect("accept ownership");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let mut startup = manager
        .begin_candidate_start(&second)
        .expect("startup reservation");
    claim_and_mark_runtime_started(&mut startup);
    let shutdown_manager = manager.clone();
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    let shutdown = thread::spawn(move || {
        shutdown_tx
            .send(shutdown_manager.shutdown(Duration::from_secs(1)))
            .expect("shutdown receiver");
    });

    let gate_state = tokio::time::timeout(Duration::from_secs(30), acceptor.changed())
        .await
        .expect("shutdown gate timeout")
        .expect("shutdown gate close");
    let status = manager.status();
    assert_eq!(
        status.active_revision,
        Some(first.revision().candidate.clone())
    );
    assert!(!status.active_accepting);
    assert!(status.candidate_revision.is_none());
    assert!(matches!(
        manager.activate(&second),
        Err(GenerationError::MutationInProgress)
    ));
    assert!(second.generation().runtime_failed());

    acceptor.acknowledge(gate_state.epoch);
    drop(ownership);
    assert!(
        shutdown_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown result")
    );
    shutdown.join().expect("shutdown thread");
    drop(startup);
}

#[test]
fn terminal_shutdown_rejects_new_mutations_but_keeps_validation_read_only() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    let active = manager.activate(&first).expect("first activation");
    let pending = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("pending candidate");

    drop(manager.begin_shutdown(Instant::now() + Duration::from_secs(1)));

    assert!(matches!(
        manager.begin_mutation(&active.revision().candidate),
        Err(GenerationError::MutationInProgress)
    ));
    assert!(matches!(
        manager.begin_config_mutation(),
        Err(GenerationError::MutationInProgress)
    ));
    assert!(matches!(
        manager.begin_candidate_start(&pending),
        Err(GenerationError::MutationInProgress)
    ));
    assert!(matches!(
        manager.prepare(document()),
        Err(GenerationError::MutationInProgress)
    ));
    assert!(matches!(
        manager.rollback(),
        Err(GenerationError::MutationInProgress)
    ));
    assert!(manager.validate_candidate(document()).is_ok());
}

#[test]
fn repeated_shutdown_retains_active_previous_and_removed_candidate() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    manager.activate(&second).expect("second activation");
    let third = manager
        .prepare(document_with_max_connections(Some(3)))
        .expect("third candidate");

    let first_shutdown = manager.reserve_shutdown();
    let second_shutdown = manager.reserve_shutdown();

    assert_eq!(first_shutdown.len(), 3);
    assert_eq!(second_shutdown.len(), 3);
    for expected in [second.generation(), first.generation(), third.generation()] {
        assert!(
            first_shutdown
                .iter()
                .any(|generation| Arc::ptr_eq(generation, expected))
        );
        assert!(
            second_shutdown
                .iter()
                .any(|generation| Arc::ptr_eq(generation, expected))
        );
    }
    assert!(manager.candidate().is_none());
}
