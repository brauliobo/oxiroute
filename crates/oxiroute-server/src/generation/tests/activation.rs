#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn activation_rollback_and_failed_activation_preserve_published_state() {
    let manager = GenerationManager::new();
    let first_candidate = manager.prepare(document()).expect("prepare first");
    let first = manager.activate(&first_candidate).expect("activate first");
    let second_candidate = manager.prepare(document()).expect("prepare second");
    let second = manager
        .activate(&second_candidate)
        .expect("activate second");

    assert!(!first.accepting());
    assert!(second.accepting());
    let rollback = manager.rollback().expect("rollback");
    assert_eq!(rollback.revision(), first.revision());
    assert!(second.accepting());
}

#[test]
fn third_activation_keeps_only_the_immediately_previous_revision_rollbackable() {
    let manager = GenerationManager::new();
    let first = manager
        .prepare(document_with_max_connections(Some(1)))
        .expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    manager.activate(&second).expect("second activation");
    let third = manager
        .prepare(document_with_max_connections(Some(3)))
        .expect("third candidate");
    manager.activate(&third).expect("third activation");

    let rollback = manager.rollback().expect("rollback candidate");

    assert_eq!(rollback.revision(), second.revision());
    assert_ne!(rollback.revision(), first.revision());
}

#[test]
fn superseded_candidate_cannot_activate_a_newer_preparation() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    let second = manager.prepare(document()).expect("second candidate");

    assert!(matches!(
        manager.activate(&first),
        Err(GenerationError::CandidateSuperseded)
    ));
    let active = manager.activate(&second).expect("second activation");
    assert!(Arc::ptr_eq(&active, second.generation()));
}

#[test]
fn publication_moves_the_admission_gate_without_overlap() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager.prepare(document()).expect("second candidate");

    assert!(first.generation().begin_admission().is_some());
    assert!(second.generation().begin_admission().is_none());
    manager.activate(&second).expect("second activation");
    assert!(first.generation().begin_admission().is_none());
    assert!(second.generation().begin_admission().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_waits_for_acceptor_quiescence() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let mut acceptor = first.generation().accept_gate().register();
    let ownership = acceptor.claim().expect("accept ownership");
    let second = manager.prepare(document()).expect("second candidate");
    let activation_manager = manager.clone();
    let activation_candidate = second.clone();
    let (activated_tx, activated_rx) = std::sync::mpsc::sync_channel(1);
    let activation = thread::spawn(move || {
        let result = activation_manager
            .activate(&activation_candidate)
            .map(|activated| Arc::ptr_eq(&activated, activation_candidate.generation()))
            .map_err(|error| error.code());
        activated_tx.send(result).expect("activation receiver");
    });

    let state = acceptor.changed().await.expect("gate close notification");
    assert!(!state.accepting);
    assert!(!second.generation().accepting());
    assert!(activated_rx.try_recv().is_err());

    acceptor.acknowledge(state.epoch);
    assert!(activated_rx.try_recv().is_err());
    drop(ownership);

    let activated_second = activated_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("activation result")
        .expect("second activation");
    assert!(activated_second);
    assert!(second.generation().accepting());
    activation.join().expect("activation thread");
}

#[tokio::test(flavor = "current_thread")]
async fn status_remains_readable_on_a_single_thread_runtime_during_publication() {
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
    let (activation_paused, release_activation) =
        pause_activation_at(&manager, ActivationHookPoint::GateClosed);
    let reads = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let reader_manager = manager.clone();
    let reader_reads = Arc::clone(&reads);
    let reader_stop = Arc::clone(&stop);
    let reader = tokio::spawn(async move {
        while !reader_stop.load(Ordering::Acquire) {
            let _ = reader_manager.status();
            reader_reads.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
        }
    });
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

    activation_paused
        .recv_timeout(Duration::from_secs(1))
        .expect("activation did not reach closed gate");
    let gate_state = tokio::time::timeout(Duration::from_secs(30), acceptor.changed())
        .await
        .expect("gate close timeout")
        .expect("gate close");
    let reads_at_close = reads.load(Ordering::Relaxed);
    tokio::time::timeout(Duration::from_secs(1), async {
        while reads.load(Ordering::Relaxed) <= reads_at_close {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("status reader did not progress while publication was paused");
    let status = manager.status();
    assert_eq!(
        status.active_revision,
        Some(first.revision().candidate.clone())
    );
    assert_eq!(
        status.candidate_revision,
        Some(second.revision().candidate.clone())
    );
    assert!(!status.active_accepting);

    acceptor.acknowledge(gate_state.epoch);
    drop(ownership);
    release_activation.wait();
    activated_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("activation result")
        .expect("activation");
    stop.store(true, Ordering::Release);
    reader.await.expect("status reader");
    activation.join().expect("activation thread");
}

#[test]
fn shutdown_close_before_activation_close_prevents_publication() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let (activation_paused, release_activation) =
        pause_activation_at(&manager, ActivationHookPoint::Reserved);
    let activation_manager = manager.clone();
    let activation_candidate = second.clone();
    let (activated, activated_rx) = mpsc::sync_channel(1);
    let activation = thread::spawn(move || {
        activated
            .send(
                activation_manager
                    .activate(&activation_candidate)
                    .map(|_| ())
                    .map_err(|error| error.code()),
            )
            .expect("activation receiver");
    });
    activation_paused
        .recv_timeout(Duration::from_secs(1))
        .expect("activation reservation pause");

    assert!(manager.shutdown(Duration::ZERO));
    release_activation.wait();

    assert_eq!(
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result"),
        Err(GenerationError::MutationInProgress.code())
    );
    activation.join().expect("activation thread");
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(!first.generation().accepting());
    assert!(!second.generation().accepting());
    assert!(manager.candidate().is_none());
    assert_eq!(manager.status().activations, 1);
}

#[test]
fn shutdown_during_closed_gate_prevents_activation_commit() {
    let manager = GenerationManager::new();
    let first = manager.prepare(document()).expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second candidate");
    let (activation_paused, release_activation) =
        pause_activation_at(&manager, ActivationHookPoint::GateClosed);
    let activation_manager = manager.clone();
    let activation_candidate = second.clone();
    let (activated, activated_rx) = mpsc::sync_channel(1);
    let activation = thread::spawn(move || {
        activated
            .send(
                activation_manager
                    .activate(&activation_candidate)
                    .map(|_| ())
                    .map_err(|error| error.code()),
            )
            .expect("activation receiver");
    });
    activation_paused
        .recv_timeout(Duration::from_secs(1))
        .expect("closed gate pause");

    assert!(manager.shutdown(Duration::ZERO));
    release_activation.wait();

    assert_eq!(
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result"),
        Err(GenerationError::MutationInProgress.code())
    );
    activation.join().expect("activation thread");
    assert!(Arc::ptr_eq(
        &manager.active().expect("active"),
        first.generation()
    ));
    assert!(!first.generation().accepting());
    assert!(!second.generation().accepting());
    assert!(manager.candidate().is_none());
    assert_eq!(manager.status().activations, 1);
}
