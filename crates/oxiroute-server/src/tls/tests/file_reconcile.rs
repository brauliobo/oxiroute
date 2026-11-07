#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn managed_acme_without_a_current_revision_uses_a_bootstrap_and_is_due() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "unused", "www.example.test", false);
    let mut config = config_with_identity(&files);
    config.certificates[0].source = CertificateSource::AcmeManaged {
        directory_url: "https://acme.example.test/directory".into(),
        state_root: temp.path().join("acme-state"),
        contacts: vec!["mailto:ops@example.test".into()],
        terms_agreed: true,
        challenge: AcmeChallengeType::Http01,
        key_type: AcmeKeyType::EcdsaP256,
        allowed_dns_suffixes: vec!["example.test".into()],
        retained_revisions: 3,
        retention_days: 30,
        dns01: None,
    };

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let [reconciler] = prepared.acme_reconcilers() else {
        panic!("one managed ACME reconciler must be prepared");
    };
    let status = reconciler.status();
    assert_eq!(status.disk_revision, "bootstrap");
    assert_eq!(status.next_action_unix_seconds, Some(0));
    assert!(reconciler.renewal_due(1));
    assert_eq!(
        prepared.certificates()["primary"]
            .snapshot()
            .metadata()
            .name,
        "primary"
    );
}

#[test]
fn direct_file_reconciliation_rotates_rolls_back_and_retains_invalid_candidates() {
    let temp = TempDir::new().unwrap();
    let initial = write_identity(temp.path(), "initial", "direct.example.test", false);
    let replacement = write_identity(temp.path(), "replacement", "direct.example.test", false);
    let mut config = config_with_identity(&initial);
    config.certificates[0].dns_names = vec!["direct.example.test".into()];
    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let [reconciler] = prepared.file_reconcilers() else {
        panic!("one direct-file reconciler must be prepared");
    };
    let active = prepared.certificates().get("primary").unwrap();
    let initial_generation = active.snapshot();
    let initial_revision = initial_generation.metadata().revision.clone();
    let initial_chain = fs::read(&initial.chain).unwrap();
    let initial_key = fs::read(&initial.key).unwrap();

    fs::copy(&replacement.chain, &initial.chain).unwrap();
    write_private_key(&initial.key, &fs::read(&replacement.key).unwrap());
    let activated = reconciler.reconcile().unwrap();
    assert!(matches!(activated, FileReconcileOutcome::Activated));
    let replacement_revision = active.snapshot().metadata().revision.clone();
    assert_ne!(replacement_revision, initial_revision);
    assert_eq!(reconciler.status().last_outcome, Some("activated"));

    fs::write(&initial.chain, b"invalid replacement").unwrap();
    let error = reconciler.reconcile().unwrap_err();
    assert!(matches!(error, FileReconcileError::InvalidCandidate { .. }));
    assert_eq!(active.snapshot().metadata().revision, replacement_revision);
    assert_eq!(
        reconciler.status().last_error_code,
        Some("invalid_candidate")
    );

    fs::copy(&replacement.chain, &initial.chain).unwrap();
    let unchanged = reconciler.reconcile().unwrap();
    assert_eq!(unchanged, FileReconcileOutcome::Unchanged);
    assert_eq!(reconciler.status().last_error_code, None);

    fs::write(&initial.chain, initial_chain).unwrap();
    write_private_key(&initial.key, &initial_key);
    let rolled_back = reconciler.reconcile().unwrap();
    assert_eq!(rolled_back, FileReconcileOutcome::Activated);
    assert_eq!(active.snapshot().metadata().revision, initial_revision);
    assert_eq!(reconciler.status().last_outcome, Some("activated"));
}

#[cfg(target_os = "linux")]
#[test]
fn direct_file_watcher_publishes_rotation_and_reports_invalid_rollback() {
    let temp = TempDir::new().unwrap();
    let initial = write_identity(temp.path(), "watch-initial", "watch.example.test", false);
    let replacement = write_identity(
        temp.path(),
        "watch-replacement",
        "watch.example.test",
        false,
    );
    let mut config = config_with_identity(&initial);
    config.certificates[0].dns_names = vec!["watch.example.test".into()];
    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let [reconciler] = prepared.file_reconcilers() else {
        panic!("one direct-file reconciler must be prepared");
    };
    let active = prepared.certificates().get("primary").unwrap();
    let initial_revision = active.snapshot().metadata().revision.clone();
    let mut watcher = FileWatcherSupervisor::start(
        vec![Arc::clone(reconciler)],
        FileWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(30),
            event_debounce: std::time::Duration::from_millis(10),
            event_max_delay: std::time::Duration::from_millis(50),
        },
    )
    .unwrap();

    fs::copy(&replacement.chain, &initial.chain).unwrap();
    write_private_key(&initial.key, &fs::read(&replacement.key).unwrap());
    wait_for_condition("direct-file rotation", || {
        active.snapshot().metadata().revision != initial_revision
    });

    fs::write(&initial.chain, b"invalid direct-file candidate").unwrap();
    wait_for_condition("direct-file invalid candidate", || {
        reconciler.status().last_error_code == Some("invalid_candidate")
            && watcher.status().degraded
    });
    let active_after_failure = active.snapshot();
    assert_ne!(active_after_failure.metadata().revision, initial_revision);
    assert!(watcher.status().reconciliation_failures > 0);

    fs::copy(&replacement.chain, &initial.chain).unwrap();
    wait_for_condition("direct-file recovery", || {
        reconciler.status().last_error_code.is_none() && !watcher.status().degraded
    });
    assert_eq!(reconciler.status().last_outcome, Some("unchanged"));
    watcher.shutdown();
}
