#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn concurrent_saves_allow_exactly_one_revision_winner() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for name in ["first", "second"] {
        let coordinator = coordinator.clone();
        let expected = expected.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            save_draft(&coordinator, &expected, rtmp_config(name))
        }));
    }
    barrier.wait();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ConfigSaveOutcome::Saved(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ConfigSaveOutcome::Conflict(_)))
            .count(),
        1
    );
    let winning_config = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ConfigSaveOutcome::Saved(document) => Some(&document.validated_config),
            _ => None,
        })
        .unwrap();
    assert_eq!(loaded(coordinator.load()).validated_config, *winning_config);
    assert_eq!(
        AuthoredRevision::from_bytes(&fs::read(path).unwrap()),
        loaded(coordinator.load()).authored_revision
    );
}

#[test]
fn independent_coordinators_serialize_the_complete_save_transaction() {
    let (temp, path, first) = fixture(&minimal_config());
    let second = CanonicalConfigCoordinator::new(&path).unwrap();
    let expected = loaded(first.load()).authored_revision;
    let first_expected = expected.clone();
    let (first_ready_tx, first_ready_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first_worker = thread::spawn(move || {
        first.save_inner(
            &first_expected,
            prepared(&first, rtmp_config("first")),
            || {
                first_ready_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
                Ok(())
            },
            || {},
            ReplaceControl::default(),
        )
    });
    first_ready_rx.recv().unwrap();

    let (second_started_tx, second_started_rx) = mpsc::channel();
    let (second_done_tx, second_done_rx) = mpsc::channel();
    let second_worker = thread::spawn(move || {
        second_started_tx.send(()).unwrap();
        let outcome = save_draft(&second, &expected, rtmp_config("second"));
        second_done_tx.send(()).unwrap();
        outcome
    });
    second_started_rx.recv().unwrap();
    let overlapped = second_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_ok();
    release_first_tx.send(()).unwrap();

    let first_outcome = first_worker.join().unwrap();
    let second_outcome = second_worker.join().unwrap();
    assert!(
        !overlapped,
        "independent coordinator bypassed the transaction lock"
    );
    assert!(matches!(first_outcome, ConfigSaveOutcome::Saved(_)));
    assert!(matches!(second_outcome, ConfigSaveOutcome::Conflict(_)));
    assert_eq!(
        loaded(CanonicalConfigCoordinator::new(&path).unwrap().load())
            .validated_config
            .as_draft(),
        &rtmp_config("first")
    );
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn separate_processes_serialize_the_complete_save_transaction() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let first_result = temp.path().join("first.result");
    let first_ready = temp.path().join("first.ready");
    let release = temp.path().join("release");
    let second_result = temp.path().join("second.result");
    let second_ready = temp.path().join("second.ready");

    let mut first = save_child(
        "holder",
        &path,
        &expected,
        &first_result,
        &first_ready,
        &release,
    );
    wait_for_path(&first_ready);
    let mut second = save_child(
        "writer",
        &path,
        &expected,
        &second_result,
        &second_ready,
        &release,
    );
    wait_for_path(&second_ready);
    thread::sleep(Duration::from_millis(100));
    let overlapped = second.try_wait().unwrap().is_some();
    fs::write(&release, []).unwrap();

    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    assert!(!overlapped, "second process bypassed the transaction lock");
    assert_eq!(fs::read_to_string(first_result).unwrap(), "saved");
    assert_eq!(fs::read_to_string(second_result).unwrap(), "conflict");
    assert_eq!(
        loaded(coordinator.load()).validated_config.as_draft(),
        &rtmp_config("child-first")
    );
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn replacing_the_lock_entry_cannot_create_an_overlapping_lock_namespace() {
    let (temp, path, first) = fixture(&minimal_config());
    let second = CanonicalConfigCoordinator::new(&path).unwrap();
    let expected = loaded(first.load()).authored_revision;
    let first_expected = expected.clone();
    let (first_ready_tx, first_ready_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first_worker = thread::spawn(move || {
        first.save_inner(
            &first_expected,
            prepared(&first, rtmp_config("first")),
            || {
                first_ready_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
                Ok(())
            },
            || {},
            ReplaceControl::default(),
        )
    });
    first_ready_rx.recv().unwrap();

    let Some(lock_path) = lock_entry(temp.path()) else {
        release_first_tx.send(()).unwrap();
        first_worker.join().unwrap();
        panic!("transaction lock entry was not created")
    };
    let replaced_lock = temp.path().join("replaced.lock");
    fs::rename(&lock_path, &replaced_lock).unwrap();
    fs::write(&lock_path, []).unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();

    let (second_done_tx, second_done_rx) = mpsc::channel();
    let second_worker = thread::spawn(move || {
        let outcome = save_draft(&second, &expected, rtmp_config("second"));
        second_done_tx.send(()).unwrap();
        outcome
    });
    let overlapped = second_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_ok();
    release_first_tx.send(()).unwrap();

    let first_outcome = first_worker.join().unwrap();
    let second_outcome = second_worker.join().unwrap();
    assert!(
        !overlapped,
        "replacement created a second active lock namespace"
    );
    let ConfigSaveOutcome::Failed(failure) = first_outcome else {
        panic!("coordinator using the replaced lock did not fail closed")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_LOCK");
    assert!(matches!(second_outcome, ConfigSaveOutcome::Saved(_)));
    assert_eq!(
        loaded(CanonicalConfigCoordinator::new(&path).unwrap().load())
            .validated_config
            .as_draft(),
        &rtmp_config("second")
    );
    assert!(no_temporary_entries(temp.path()));
}
