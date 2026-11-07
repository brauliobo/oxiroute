#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
#[test]
fn reconciles_valid_forward_activation_unchanged_revision_and_operator_rollback() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    write_new_certbot_revision(temp.path(), &files, 2, "forward", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let initial_generation_revision = initial.generation().metadata().revision.clone();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    );

    set_live_revision(&files, 2);
    let forward = reconciler.reconcile().unwrap();
    assert_eq!(
        forward,
        CertbotReconcileOutcome::Activated {
            previous_archive_revision: 1,
            archive_revision: 2,
            direction: CertbotActivationDirection::Forward,
        }
    );
    let forward_generation_revision = active.snapshot().metadata().revision.clone();
    assert_ne!(forward_generation_revision, initial_generation_revision);

    assert_eq!(
        reconciler.reconcile().unwrap(),
        CertbotReconcileOutcome::Unchanged {
            archive_revision: 2,
        }
    );

    set_live_revision(&files, 1);
    assert_eq!(
        reconciler.reconcile().unwrap(),
        CertbotReconcileOutcome::Activated {
            previous_archive_revision: 2,
            archive_revision: 1,
            direction: CertbotActivationDirection::Rollback,
        }
    );
    assert_eq!(
        active.snapshot().metadata().revision,
        initial_generation_revision
    );
}

#[cfg(unix)]
#[test]
fn reconciles_changed_valid_material_at_the_same_archive_revision() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let initial_generation_revision = initial.generation().metadata().revision.clone();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    );
    write_new_certbot_revision(
        temp.path(),
        &files,
        1,
        "same-revision",
        "certbot.example.test",
    );

    assert_eq!(
        reconciler.reconcile().unwrap(),
        CertbotReconcileOutcome::Activated {
            previous_archive_revision: 1,
            archive_revision: 1,
            direction: CertbotActivationDirection::Replacement,
        }
    );
    assert_ne!(
        active.snapshot().metadata().revision,
        initial_generation_revision
    );
}

#[cfg(unix)]
#[test]
fn unchanged_check_cannot_leave_a_concurrent_external_generation_active() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let source_generation_revision = initial.generation().metadata().revision.clone();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    );
    let external_files = write_identity(
        temp.path(),
        "external-unchanged",
        "certbot.example.test",
        false,
    );
    let external = Arc::new(
        CertificateGeneration::from_files(
            "primary",
            &["certbot.example.test".into()],
            &external_files.chain,
            &external_files.key,
        )
        .unwrap(),
    );
    let mut intervened = false;

    let outcome = reconciler
        .reconcile_with_before_publish(|attempt| {
            if attempt == 0 {
                intervened = true;
                let current = active.snapshot();
                active
                    .publish_if_current(&current, Arc::clone(&external))
                    .unwrap();
            }
        })
        .unwrap();

    assert!(intervened);
    assert!(matches!(
        outcome,
        CertbotReconcileOutcome::Activated {
            archive_revision: 1,
            direction: CertbotActivationDirection::Replacement,
            ..
        }
    ));
    assert_eq!(
        active.snapshot().metadata().revision,
        source_generation_revision
    );
}

#[cfg(unix)]
#[test]
fn retains_the_active_generation_during_sequential_certbot_link_updates() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    write_new_certbot_revision(temp.path(), &files, 2, "sequential", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let initial = active.snapshot();
    let reconciler = CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    );

    for stem in ["cert", "chain", "fullchain"] {
        set_live_link(&files, &format!("{stem}.pem"), &format!("{stem}2.pem"));
        let error = reconciler.reconcile().unwrap_err();
        assert!(matches!(
            error,
            CertbotReconcileError::InvalidCandidate {
                active_archive_revision: 1,
                ..
            }
        ));
        assert!(Arc::ptr_eq(&active.snapshot(), &initial));
        let status: CertbotReconcilerStatus = reconciler.status();
        assert_eq!(status.last_outcome, None);
        assert_eq!(status.last_error_code, Some("invalid_candidate"));
    }

    set_live_link(&files, "privkey.pem", "privkey2.pem");
    assert!(matches!(
        reconciler.reconcile().unwrap(),
        CertbotReconcileOutcome::Activated {
            archive_revision: 2,
            ..
        }
    ));
    assert!(!Arc::ptr_eq(&active.snapshot(), &initial));
    let status = reconciler.status();
    assert_eq!(status.certificate, "primary");
    assert_eq!(status.active_archive_revision, 2);
    assert_eq!(
        status.active_content_revision,
        active.snapshot().metadata().revision
    );
    assert_eq!(
        status.not_after,
        active.snapshot().metadata().validity.not_after
    );
    assert_eq!(status.last_outcome, Some("activated_forward"));
    assert_eq!(status.last_error_code, None);
}

#[cfg(unix)]
#[test]
fn retains_the_active_generation_when_a_common_certbot_revision_is_invalid() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    copy_certbot_revision(&files.archive, 1, 2, false);
    fs::write(files.archive.join("fullchain2.pem"), b"invalid candidate").unwrap();
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let initial = active.snapshot();
    let reconciler = CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    );

    set_live_revision(&files, 2);
    let error = reconciler.reconcile().unwrap_err();

    assert!(matches!(
        error,
        CertbotReconcileError::InvalidCandidate {
            active_archive_revision: 1,
            ..
        }
    ));
    assert!(Arc::ptr_eq(&active.snapshot(), &initial));
    assert_eq!(reconciler.active_archive_revision(), 1);
}

#[cfg(unix)]
#[test]
fn rereads_the_certbot_lineage_after_a_cas_conflict() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    write_new_certbot_revision(
        temp.path(),
        &files,
        2,
        "conflict-two",
        "certbot.example.test",
    );
    write_new_certbot_revision(
        temp.path(),
        &files,
        3,
        "conflict-three",
        "certbot.example.test",
    );
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = CertbotReconciler::new(
        lineage.clone(),
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    );
    let external_files = write_identity(
        temp.path(),
        "external-conflict",
        "certbot.example.test",
        false,
    );
    let external = Arc::new(
        CertificateGeneration::from_files(
            "primary",
            &["certbot.example.test".into()],
            &external_files.chain,
            &external_files.key,
        )
        .unwrap(),
    );
    set_live_revision(&files, 2);

    let mut conflicted = false;
    let outcome = reconciler
        .reconcile_with_before_publish(|attempt| {
            if attempt == 0 {
                conflicted = true;
                let current = active.snapshot();
                active
                    .publish_if_current(&current, Arc::clone(&external))
                    .unwrap();
                set_live_revision(&files, 3);
            }
        })
        .unwrap();

    assert!(conflicted);
    assert!(matches!(
        outcome,
        CertbotReconcileOutcome::Activated {
            archive_revision: 3,
            ..
        }
    ));
    let third = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    assert_eq!(
        active.snapshot().metadata().revision,
        third.generation().metadata().revision
    );
}

#[cfg(unix)]
#[test]
fn periodic_certbot_rescan_activates_and_shutdown_prevents_later_publication() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    write_new_certbot_revision(temp.path(), &files, 2, "periodic", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let initial_generation_revision = initial.generation().metadata().revision.clone();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = Arc::new(CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    ));
    let mut supervisor = CertbotWatcherSupervisor::start(
        vec![Arc::clone(&reconciler)],
        CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(1),
            event_debounce: std::time::Duration::from_millis(10),
            event_max_delay: std::time::Duration::from_millis(50),
        },
    )
    .unwrap();
    let monitor: CertbotWatcherMonitor = supervisor.monitor();
    assert!(monitor.status().running);

    set_live_revision(&files, 2);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while reconciler.active_archive_revision() != 2 && std::time::Instant::now() < deadline {
        thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(reconciler.active_archive_revision(), 2);
    assert_ne!(
        active.snapshot().metadata().revision,
        initial_generation_revision
    );
    let periodic_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while supervisor.status().periodic_rescans == 0 && std::time::Instant::now() < periodic_deadline
    {
        thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(supervisor.status().periodic_rescans > 0);

    supervisor.shutdown();
    assert!(!monitor.status().running);
    let active_after_shutdown = active.snapshot();
    set_live_revision(&files, 1);
    thread::sleep(std::time::Duration::from_millis(50));

    assert!(Arc::ptr_eq(&active.snapshot(), &active_after_shutdown));
    assert_eq!(reconciler.active_archive_revision(), 2);
}
