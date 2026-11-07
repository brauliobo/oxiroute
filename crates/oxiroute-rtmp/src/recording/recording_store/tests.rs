use std::{fs, sync::mpsc};

use super::*;
use tempfile::tempdir;

#[test]
fn finalizer_runs_one_root_job_at_a_time() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024),
            max_files: Some(8),
            max_active_recorders: 4,
        },
    )
    .expect("recording store");
    assert_eq!(
        store
            .shared()
            .expect("active recording store")
            .finalizer
            .threads
            .len(),
        MAX_FINALIZER_THREADS
    );
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (started_tx, started_rx) = mpsc::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let authority = RecordingFinalizerAuthority {
        incarnation: store.handle.incarnation,
        shared: store.shared().expect("active recording store"),
    };

    for job in 0..4 {
        let release = Arc::clone(&release);
        let started_tx = started_tx.clone();
        let completed_tx = completed_tx.clone();
        authority.submit(Box::new(move || {
            started_tx.send(job).expect("report started finalization");
            let (lock, available) = &*release;
            drop(
                available
                    .wait_while(lock.lock().expect("release mutex poisoned"), |released| {
                        !*released
                    })
                    .expect("release mutex poisoned while waiting"),
            );
            completed_tx
                .send(job)
                .expect("report completed finalization");
        }));
    }

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first finalizer worker");
    assert!(
        started_rx.recv_timeout(Duration::from_millis(40)).is_err(),
        "more than one root finalization ran concurrently"
    );

    *release.0.lock().expect("release mutex poisoned") = true;
    release.1.notify_all();
    for _ in 0..3 {
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("queued finalization started");
    }
    for _ in 0..4 {
        completed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("finalization completed");
    }
}

#[test]
fn bounded_reopen_registration_expires_without_registering_a_new_incarnation() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let gate = store.install_operation_gate(RecordingOperationGateKind::CreateBeforeOpen);
    let creating_store = store.clone();
    let creator = thread::spawn(move || creating_store.create("blocked.flv"));
    assert!(gate.wait_reached(Duration::from_secs(1)));
    let shared = store.shared().expect("shared store");
    store.retire();

    let started = Instant::now();
    let result = RecordingStore::open_with_deadline(
        root.path(),
        test_limits(),
        Some(started + Duration::from_millis(50)),
    );

    assert!(matches!(
        result,
        Err(RecordingStoreError::CreationCancelled)
    ));
    assert!(started.elapsed() >= Duration::from_millis(35));
    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(shared.incarnation_count(), 1);
    gate.release();
    assert!(matches!(
        creator.join().expect("creator"),
        Err(RecordingStoreError::CreationCancelled)
    ));
    assert_eq!(shared.incarnation_count(), 0);
}

#[test]
fn bounded_reopen_rejects_a_late_claim_release_without_inserting() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let shared = store.shared().expect("shared store");
    let claim = shared
        .claim(store.handle.incarnation)
        .expect("inactive claim fixture");
    let before_wait_gate =
        store.install_operation_gate(RecordingOperationGateKind::RegisterBeforeWait);
    store.retire();

    let root_path = root.path().to_owned();
    let (opened_tx, opened_rx) = mpsc::sync_channel(1);
    let deadline = Instant::now() + Duration::from_millis(100);
    let opener = thread::spawn(move || {
        opened_tx
            .send(RecordingStore::open_with_deadline(
                root_path,
                test_limits(),
                Some(deadline),
            ))
            .expect("bounded opener result receiver");
    });

    assert!(before_wait_gate.wait_reached(Duration::from_secs(1)));
    let registration_gate =
        shared.install_operation_gate(RecordingOperationGateKind::RegisterAfterWake);
    before_wait_gate.release();
    drop(claim);
    assert!(registration_gate.wait_reached(Duration::from_secs(1)));
    thread::sleep(Duration::from_millis(150));
    registration_gate.release();

    assert!(matches!(
        opened_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("late bounded opener"),
        Err(RecordingStoreError::CreationCancelled)
    ));
    opener.join().expect("opener");
    assert_eq!(shared.incarnation_count(), 0);

    let reopened = test_store(root.path());
    reopened
        .create("fresh.flv")
        .expect("reopen after late wake");
}

#[test]
fn retired_incarnation_preserves_late_finalization_as_recoverable_partial() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let mut recording = store.create("staging.flv").expect("recording");
    recording.write_all(b"recoverable").expect("recording data");
    let partial = recording.partial_relative_name().to_owned();
    recording
        .set_final_relative_name("camera.flv".to_owned())
        .expect("final name");
    let authority = recording.finalizer_authority();
    let (result_tx, result_rx) = mpsc::sync_channel(1);

    store.retire();
    let reopened = test_store(root.path());
    let ticket = authority.submit(Box::new(move || {
        result_tx
            .send(recording.commit())
            .expect("late finalization result");
    }));

    let error = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("late finalization")
        .expect_err("retired incarnation cannot publish");
    assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
    assert!(root.path().join(&partial).is_file());
    assert!(!root.path().join("camera.flv").exists());
    assert_eq!(reopened.stats().bytes_used, 11);
    assert_eq!(reopened.stats().files, 1);
    assert!(!ticket.cancel_queued());
    let fresh = reopened
        .create("camera.flv")
        .expect("fresh generation recording");
    drop(fresh);
    assert!(root.path().join(&partial).is_file());
}

#[test]
fn retirement_before_publication_claim_reopens_with_only_the_recoverable_partial() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let mut recording = store.create("staging.flv").expect("recording");
    recording.write_all(b"recoverable").expect("recording data");
    let partial = recording.partial_relative_name().to_owned();
    recording
        .set_final_relative_name("camera.flv".to_owned())
        .expect("final name");
    let gate = recording.commit_cancellation().install_publication_gate();
    let (committed_tx, committed_rx) = mpsc::sync_channel(1);
    let committer = thread::spawn(move || {
        committed_tx
            .send(recording.commit())
            .expect("commit result receiver");
    });
    assert!(gate.wait_before_claim(Duration::from_secs(1)));

    store.retire();
    let reopened = test_store(root.path());
    gate.allow_claim();

    let error = committed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("retired commit result")
        .expect_err("retirement wins before publication claim");
    assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
    assert!(root.path().join(&partial).is_file());
    assert!(!root.path().join("camera.flv").exists());
    assert_eq!(reopened.stats().bytes_used, 11);
    assert_eq!(reopened.stats().files, 1);
    committer.join().expect("committer");
}

#[test]
fn retirement_fences_create_before_open_and_reopen_waits_for_the_claim() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let gate = store.install_operation_gate(RecordingOperationGateKind::CreateBeforeOpen);
    let creating_store = store.clone();
    let (created_tx, created_rx) = mpsc::sync_channel(1);
    let creator = thread::spawn(move || {
        created_tx
            .send(creating_store.create("old.flv"))
            .expect("create result receiver");
    });
    assert!(gate.wait_reached(Duration::from_secs(1)));

    store.retire();
    let root_path = root.path().to_owned();
    let (reopened_tx, reopened_rx) = mpsc::sync_channel(1);
    let opener = thread::spawn(move || {
        reopened_tx
            .send(test_store(&root_path))
            .expect("reopened store receiver");
    });
    assert!(matches!(
        reopened_rx.recv_timeout(Duration::from_millis(40)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    gate.release();
    assert!(matches!(
        created_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("retired create result"),
        Err(RecordingStoreError::CreationCancelled)
    ));
    let reopened = reopened_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reopen after create claim");
    assert_eq!(reopened.stats(), RecordingStoreStats::default());
    assert!(!root.path().join("old.flv").exists());
    reopened.create("fresh.flv").expect("fresh create");
    creator.join().expect("creator");
    opener.join().expect("opener");
}

#[test]
fn retirement_fences_resume_after_shared_capture() {
    let root = tempdir().expect("recording root");
    let mut flv = Vec::from(&b"FLV\x01\x04\0\0\0\x09\0\0\0\0"[..]);
    flv.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    flv.extend_from_slice(&11_u32.to_be_bytes());
    fs::write(root.path().join("camera.flv"), &flv).expect("existing FLV");
    let store = test_store(root.path());
    let gate = store.install_operation_gate(RecordingOperationGateKind::ResumeAfterSharedCapture);
    let resuming_store = store.clone();
    let (resumed_tx, resumed_rx) = mpsc::sync_channel(1);
    let resumer = thread::spawn(move || {
        resumed_tx
            .send(resuming_store.resume_with_options("camera.flv", false, None))
            .expect("resume result receiver");
    });
    assert!(gate.wait_reached(Duration::from_secs(1)));

    store.retire();
    let reopened = test_store(root.path());
    gate.release();

    assert!(matches!(
        resumed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("retired resume result"),
        Err(RecordingStoreError::CreationCancelled)
    ));
    assert_eq!(
        reopened.stats().bytes_used,
        u64::try_from(flv.len()).unwrap()
    );
    assert_eq!(reopened.stats().files, 1);
    assert_eq!(fs::read(root.path().join("camera.flv")).unwrap(), flv);
    resumer.join().expect("resumer");
}

#[test]
fn claimed_publication_finishes_before_reopen_without_overwrite() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let mut recording = store.create("staging.flv").expect("recording");
    recording.write_all(b"old").expect("old data");
    recording
        .set_final_relative_name("camera.flv".to_owned())
        .expect("final name");
    let cancellation = recording.commit_cancellation();
    let gate = cancellation.install_publication_gate();
    let (committed_tx, committed_rx) = mpsc::sync_channel(1);
    let committer = thread::spawn(move || {
        committed_tx
            .send(recording.commit())
            .expect("commit result receiver");
    });
    assert!(gate.wait_before_claim(Duration::from_secs(1)));
    gate.allow_claim();
    assert!(gate.wait_after_claim(Duration::from_secs(1)));

    store.retire();
    let root_path = root.path().to_owned();
    let (reopened_tx, reopened_rx) = mpsc::sync_channel(1);
    let opener = thread::spawn(move || {
        reopened_tx
            .send(test_store(&root_path))
            .expect("reopened store receiver");
    });
    assert!(matches!(
        reopened_rx.recv_timeout(Duration::from_millis(40)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    gate.allow_publication();
    let commit = committed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("claimed publication")
        .expect("old claim publishes before reopen");
    assert_eq!(commit.relative_name, "camera.flv");
    let reopened = reopened_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reopen after publication claim");
    let mut fresh = reopened
        .create("camera.flv")
        .expect("fresh collision create");
    fresh.write_all(b"fresh").expect("fresh data");
    let fresh = fresh.commit().expect("fresh commit");
    assert_eq!(fresh.relative_name, "camera-1.flv");
    assert_eq!(fs::read(root.path().join("camera.flv")).unwrap(), b"old");
    assert_eq!(
        fs::read(root.path().join("camera-1.flv")).unwrap(),
        b"fresh"
    );
    assert_eq!(reopened.stats().bytes_used, 8);
    assert_eq!(reopened.stats().files, 2);
    committer.join().expect("committer");
    opener.join().expect("opener");
}

#[test]
fn blocked_startup_scan_does_not_block_an_unrelated_root() {
    let blocked_root = tempdir().expect("blocked recording root");
    let unrelated_root = tempdir().expect("unrelated recording root");
    let ownership = fs::File::open(blocked_root.path()).expect("blocked root ownership");
    rustix_fs::flock(&ownership, FlockOperation::LockExclusive).expect("stall blocked root scan");
    let blocked_path = blocked_root.path().to_owned();
    let (blocked_tx, blocked_rx) = mpsc::channel();
    let blocked_opener = thread::spawn(move || {
        let store = RecordingStore::open(blocked_path, test_limits()).expect("blocked store");
        blocked_tx
            .send(store.stats())
            .expect("report blocked store");
    });
    assert!(matches!(
        blocked_rx.recv_timeout(Duration::from_millis(40)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let unrelated_path = unrelated_root.path().to_owned();
    let (unrelated_tx, unrelated_rx) = mpsc::channel();
    let unrelated_opener = thread::spawn(move || {
        let store = RecordingStore::open(unrelated_path, test_limits()).expect("unrelated store");
        unrelated_tx
            .send(store.stats())
            .expect("report unrelated store");
    });
    let unrelated_result = unrelated_rx.recv_timeout(Duration::from_secs(1));

    rustix_fs::flock(&ownership, FlockOperation::Unlock).expect("release blocked root scan");
    assert_eq!(
        blocked_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocked store opens after release"),
        RecordingStoreStats::default()
    );
    blocked_opener.join().expect("blocked store opener");
    unrelated_opener.join().expect("unrelated store opener");
    assert_eq!(
        unrelated_result.expect("unrelated root opens without waiting"),
        RecordingStoreStats::default()
    );
}

#[test]
fn resumed_file_drop_releases_accounting_when_ownership_is_lost() {
    let root = tempdir().expect("recording root");
    let mut flv = Vec::from(&b"FLV\x01\x04\0\0\0\x09\0\0\0\0"[..]);
    flv.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    flv.extend_from_slice(&11_u32.to_be_bytes());
    fs::write(root.path().join("camera.flv"), &flv).expect("existing FLV");
    let store = test_store(root.path());
    let resumed = store
        .resume_with_options("camera.flv", false, None)
        .expect("resumed recording");

    fs::remove_file(root.path().join("camera.flv")).expect("remove resumed path");
    drop(resumed);

    assert_eq!(store.stats(), RecordingStoreStats::default());
}

#[test]
fn reports_partial_and_published_name_when_publication_rollback_fails() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024),
            max_files: Some(4),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let mut recording = store.create("recording.flv").expect("recording");
    recording.write_all(b"recording").expect("recording data");
    let partial = recording.partial_relative_name().to_owned();
    recording
        .set_final_relative_name("camera.flv".to_owned())
        .expect("final recording name");
    recording.fail_publication_unlinks();

    let error = recording.commit().expect_err("injected rollback failure");

    assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
    assert_eq!(
        error.published_recording(),
        Some(&RecordingCommit {
            relative_name: "camera.flv".to_owned(),
            bytes: 9,
        })
    );
    assert!(root.path().join(&partial).is_file());
    assert!(root.path().join("camera.flv").is_file());
    assert_eq!(
        store.stats(),
        RecordingStoreStats {
            bytes_used: 18,
            files: 2,
            active_recorders: 0,
        }
    );
    assert!(matches!(
        error,
        RecordingStoreError::PublishRollback {
            recording: RecordingCommit { bytes: 9, .. },
            ..
        }
    ));
}

#[test]
fn synchronizes_a_successful_publication_rollback_before_reporting_the_partial() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let mut recording = store.create("recording.flv").expect("recording");
    recording.write_all(b"recording").expect("recording data");
    let partial = recording.partial_relative_name().to_owned();
    recording
        .set_final_relative_name("camera.flv".to_owned())
        .expect("final recording name");
    recording.fail_partial_unlink();

    let error = recording
        .commit()
        .expect_err("injected partial unlink failure");

    assert!(matches!(error, RecordingStoreError::Publish { .. }));
    assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
    assert_eq!(error.published_recording(), None);
    assert!(root.path().join(partial).is_file());
    assert!(!root.path().join("camera.flv").exists());
    assert_eq!(
        store.stats(),
        RecordingStoreStats {
            bytes_used: 9,
            files: 1,
            active_recorders: 0,
        }
    );
}

#[test]
fn reports_conservative_names_when_rollback_directory_sync_fails() {
    let root = tempdir().expect("recording root");
    let store = test_store(root.path());
    let mut recording = store.create("recording.flv").expect("recording");
    recording.write_all(b"recording").expect("recording data");
    let partial = recording.partial_relative_name().to_owned();
    recording
        .set_final_relative_name("camera.flv".to_owned())
        .expect("final recording name");
    recording.fail_partial_unlink_and_rollback_sync();

    let error = recording
        .commit()
        .expect_err("injected rollback directory sync failure");

    assert!(matches!(
        error,
        RecordingStoreError::PublishRollbackDirectorySync { .. }
    ));
    assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
    assert_eq!(
        error.published_recording(),
        Some(&RecordingCommit {
            relative_name: "camera.flv".to_owned(),
            bytes: 9,
        })
    );
    assert!(root.path().join(partial).is_file());
    assert!(!root.path().join("camera.flv").exists());
    assert_eq!(
        store.stats(),
        RecordingStoreStats {
            bytes_used: 9,
            files: 1,
            active_recorders: 0,
        }
    );
}

fn test_store(root: &Path) -> RecordingStore {
    RecordingStore::open(root, test_limits()).expect("recording store")
}

fn test_limits() -> RecordingStoreLimits {
    RecordingStoreLimits {
        max_bytes: Some(1024),
        max_files: Some(4),
        max_active_recorders: 1,
    }
}
