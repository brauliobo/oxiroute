#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn dns01_issues_wildcard_and_cleans_the_exact_provider_record() {
    let temp = TempDir::new().expect("state directory");
    let credentials = temp.path().join("dns-credentials");
    fs::write(&credentials, b"dns-secret").expect("credentials");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600))
            .expect("credential permissions");
    }
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    let revisions = RevisionStore::from_arc(Arc::clone(&state));
    let names = vec!["*.example.test".to_owned()];
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed-wildcard",
        &names,
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
    let provider = Arc::new(FakeDnsProvider::default());
    let created = Arc::clone(&provider.created);
    let propagated = Arc::clone(&provider.propagated);
    let cleaned = Arc::clone(&provider.cleaned);
    let provider: Arc<dyn Dns01Provider> = provider;
    let reconciler = AcmeManagedReconciler::new_with_dns_provider(
        "managed-wildcard",
        names,
        AcmeManagedPolicy {
            directory_url: "https://acme.test/directory".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::Dns01,
            key_type: AcmeKeyType::EcdsaP256,
            allowed_dns_suffixes: vec!["example.test".into()],
            retained_revisions: 3,
            retention_days: 30,
            dns01: Some(AcmeDns01Config {
                provider: "fake".into(),
                credential_file: credentials,
                timeout_seconds: 30,
            }),
        },
        revisions.clone(),
        "bootstrap".into(),
        None,
        None,
        true,
        ChallengeStore::default(),
        Some(provider),
        Arc::clone(&active),
    );
    let transport = FakePebbleTransport {
        responses: Arc::new(Mutex::new(VecDeque::from([
            directory_response(),
            nonce_response("nonce-account"),
            account_response(),
            order_response_with_identifier("*.example.test", "pending"),
            dns_authorization_response("pending"),
            challenge_response(),
            dns_authorization_response("valid"),
            order_response_with_identifier("*.example.test", "processing"),
            order_response_with_identifier("*.example.test", "valid"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        ca: Arc::new(test_ca().expect("CA")),
        certificate: Arc::new(Mutex::new(None)),
        certificate_names: vec!["*.example.test".into()],
    };
    let outcome = reconciler
        .renew_with_transport(transport)
        .expect("managed DNS-01 issuance");

    assert_eq!(outcome, AcmeManagedOutcome::Activated);
    assert_eq!(reconciler.status().challenge, "dns01");
    assert_eq!(reconciler.status().dns_provider.as_deref(), Some("fake"));
    assert_eq!(
        created.lock().expect("created records").as_slice(),
        ["_acme-challenge.example.test".to_owned()]
    );
    assert_eq!(
        propagated.lock().expect("propagated records").as_slice(),
        ["fake-record-1".to_owned()]
    );
    assert_eq!(
        cleaned.lock().expect("cleaned records").as_slice(),
        ["fake-record-1".to_owned()]
    );
    assert!(revisions.load_current("managed-wildcard").is_ok());
    let persisted = persisted_jobs(&state);
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].status(), &JobStatus::Succeeded);
}

#[test]
fn restart_recovers_a_durable_dns_cleanup_journal_and_retries_failures() {
    let temp = TempDir::new().expect("state directory");
    let credentials = temp.path().join("dns-credentials");
    fs::write(&credentials, b"dns-secret").expect("credentials");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600))
            .expect("credential permissions");
    }
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    let journal = PersistedDnsCleanup {
        certificate: "managed-recovery".into(),
        provider: "fake".into(),
        identifier: "*.example.test".into(),
        challenge_url: "https://acme.test/challenge/1".into(),
        record_name: "_acme-challenge.example.test".into(),
        record_value: "txt-value".into(),
        provider_record_id: "fake-record-1".into(),
    };
    state
        .write_secret_json("certificates/managed-recovery/dns-cleanup.json", &journal)
        .expect("cleanup journal");
    let revisions = RevisionStore::from_arc(Arc::clone(&state));
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed-recovery",
        &["*.example.test".into()],
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
    let provider = Arc::new(FakeDnsProvider::default());
    let cleanup_fail = Arc::clone(&provider.cleanup_fail);
    let cleaned = Arc::clone(&provider.cleaned);
    let reconciler = AcmeManagedReconciler::new_with_dns_provider(
        "managed-recovery",
        vec!["*.example.test".into()],
        AcmeManagedPolicy {
            directory_url: "https://acme.test/directory".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::Dns01,
            key_type: AcmeKeyType::EcdsaP256,
            allowed_dns_suffixes: vec!["example.test".into()],
            retained_revisions: 3,
            retention_days: 30,
            dns01: Some(AcmeDns01Config {
                provider: "fake".into(),
                credential_file: credentials,
                timeout_seconds: 30,
            }),
        },
        revisions.clone(),
        "bootstrap".into(),
        None,
        None,
        true,
        ChallengeStore::default(),
        Some(provider),
        Arc::clone(&active),
    );
    assert_eq!(
        cleaned.lock().expect("cleaned records").as_slice(),
        ["fake-record-1".to_owned()]
    );
    assert!(
        !temp
            .path()
            .join("state/certificates/managed-recovery/dns-cleanup.json")
            .exists()
    );
    assert_eq!(reconciler.status().dns_cleanup_status, "recovered");
    assert_eq!(reconciler.status().dns_provider_health, Some("healthy"));

    state
        .write_secret_json("certificates/managed-recovery/dns-cleanup.json", &journal)
        .expect("second cleanup journal");
    cleanup_fail.store(true, Ordering::Release);
    assert_eq!(
        reconciler.recover_pending_dns_cleanup(),
        AcmeDnsCleanupRecovery::Deferred
    );
    assert_eq!(reconciler.status().dns_cleanup_status, "failed");
    assert!(
        temp.path()
            .join("state/certificates/managed-recovery/dns-cleanup.json")
            .is_file()
    );
    cleanup_fail.store(false, Ordering::Release);
    assert_eq!(
        reconciler.recover_pending_dns_cleanup(),
        AcmeDnsCleanupRecovery::Recovered
    );
}

#[test]
fn cancelled_dns_recovery_retains_journal_until_one_confirmed_cleanup() {
    let temp = TempDir::new().expect("state directory");
    let credentials = temp.path().join("dns-credentials");
    fs::write(&credentials, b"dns-secret").expect("credentials");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600))
            .expect("credential permissions");
    }
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    let revisions = RevisionStore::from_arc(Arc::clone(&state));
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed-recovery",
        &["*.example.test".into()],
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let reached = Arc::new(Barrier::new(2));
    let cleaned = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(CancellableCleanupProvider {
        reached: Arc::clone(&reached),
        cleaned: Arc::clone(&cleaned),
        block: AtomicBool::new(true),
    });
    let reconciler = Arc::new(AcmeManagedReconciler::new_with_dns_provider(
        "managed-recovery",
        vec!["*.example.test".into()],
        AcmeManagedPolicy {
            directory_url: "https://acme.test/directory".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::Dns01,
            key_type: AcmeKeyType::EcdsaP256,
            allowed_dns_suffixes: vec!["example.test".into()],
            retained_revisions: 3,
            retention_days: 30,
            dns01: Some(AcmeDns01Config {
                provider: "fake".into(),
                credential_file: credentials,
                timeout_seconds: 30,
            }),
        },
        revisions,
        "bootstrap".into(),
        None,
        None,
        true,
        ChallengeStore::default(),
        Some(provider),
        Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap))),
    ));
    let journal_path = "certificates/managed-recovery/dns-cleanup.json";
    state
        .write_secret_json(
            journal_path,
            &PersistedDnsCleanup {
                certificate: "managed-recovery".into(),
                provider: "fake".into(),
                identifier: "*.example.test".into(),
                challenge_url: "https://acme.test/challenge/1".into(),
                record_name: "_acme-challenge.example.test".into(),
                record_value: "txt-value".into(),
                provider_record_id: "fake-record-1".into(),
            },
        )
        .expect("cleanup journal");
    let cancellation = Dns01Cancellation::new();
    let operation = AcmeOperation::with_deadline(
        cancellation.clone(),
        std::time::Instant::now() + Duration::from_secs(2),
    );
    let worker_reconciler = Arc::clone(&reconciler);
    let worker = std::thread::spawn(move || {
        worker_reconciler.recover_pending_dns_cleanup_with_operation(&operation)
    });
    reached.wait();

    cancellation.cancel();
    let cancelled = worker.join().expect("cleanup worker");

    assert!(matches!(
        cancelled,
        Err(AcmeManagedError::Protocol(AcmeError::Cancelled))
    ));
    assert_eq!(cleaned.load(Ordering::Acquire), 0);
    assert!(state.root().join(journal_path).is_file());
    assert_ne!(reconciler.status().dns_cleanup_status, "recovered");

    assert_eq!(
        reconciler.recover_pending_dns_cleanup(),
        AcmeDnsCleanupRecovery::Recovered
    );
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
    assert!(!state.root().join(journal_path).exists());
    assert_eq!(reconciler.status().dns_cleanup_status, "recovered");
    assert_eq!(
        reconciler.recover_pending_dns_cleanup(),
        AcmeDnsCleanupRecovery::NotPending
    );
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
}
