#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn restart_retains_bounded_retry_state_and_redacted_error_code() {
    let temp = TempDir::new().expect("state directory");
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    state
        .write_json(
            "certificates/managed/renewal.json",
            &PersistedRenewal {
                certificate: "managed".into(),
                identifiers: vec!["proxy.example.test".into()],
                directory_url: "https://acme.test/directory".into(),
                account_url: Some("https://acme.test/acme/acct/1".into()),
                authenticator: "http01".into(),
                dns_provider: None,
                key_type: "ecdsa_p256".into(),
                next_action_unix_seconds: Some(2_000),
                retry_at_unix_seconds: Some(2_000),
                retry_attempt: 2,
                suggested_renewal_unix_seconds: None,
                auto_retry_blocked: false,
                renewal_info_url: None,
                last_success_unix_seconds: Some(1_000),
                last_error_code: Some("transport_failed".into()),
                renewal_information_status: "not_advertised".into(),
                paused: false,
            },
        )
        .expect("renewal state");
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed",
        &["proxy.example.test".into()],
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
    let reconciler = AcmeManagedReconciler::new(
        "managed",
        vec!["proxy.example.test".into()],
        AcmeManagedPolicy {
            directory_url: "https://acme.test/directory".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::Http01,
            key_type: AcmeKeyType::EcdsaP256,
            allowed_dns_suffixes: vec!["example.test".into()],
            retained_revisions: 3,
            retention_days: 30,
            dns01: None,
        },
        RevisionStore::from_arc(state),
        "revision".into(),
        Some(900),
        Some(4_000),
        false,
        ChallengeStore::default(),
        active,
    );

    let status = reconciler.status();
    assert_eq!(status.retry_attempt, 2);
    assert_eq!(status.next_action_unix_seconds, Some(2_000));
    assert_eq!(status.last_success_unix_seconds, Some(1_000));
    assert_eq!(status.last_error_code.as_deref(), Some("transport_failed"));
}

#[test]
fn cancellation_at_transport_boundaries_persists_one_cancelled_job_and_cleans_once() {
    for boundary in [
        BlockingBoundary::Connect,
        BlockingBoundary::Write,
        BlockingBoundary::Read,
        BlockingBoundary::Poll,
    ] {
        let temp = TempDir::new().expect("state directory");
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        let names = vec!["proxy.example.test".to_owned()];
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed",
            &names,
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let reconciler = Arc::new(AcmeManagedReconciler::new(
            "managed",
            names,
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                challenge: AcmeChallengeType::Http01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: None,
            },
            RevisionStore::from_arc(Arc::clone(&state)),
            "bootstrap".into(),
            None,
            None,
            true,
            ChallengeStore::default(),
            Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap))),
        ));
        let reached = Arc::new(Barrier::new(2));
        let cleanup = Arc::new(AtomicUsize::new(0));
        let cancellation = Dns01Cancellation::new();
        let operation = AcmeOperation::with_deadline(
            cancellation.clone(),
            std::time::Instant::now() + Duration::from_secs(2),
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let worker_reached = Arc::clone(&reached);
        let worker_cleanup = Arc::clone(&cleanup);
        let worker = std::thread::spawn(move || {
            worker_reconciler.renew_with_transport_and_operation(
                BlockingTransport {
                    boundary,
                    reached: worker_reached,
                    cleanup: worker_cleanup,
                },
                operation,
            )
        });
        reached.wait();

        cancellation.cancel();
        let result = worker.join().expect("renewal worker");

        assert!(matches!(
            result,
            Err(AcmeManagedError::Protocol(AcmeError::Cancelled))
        ));
        assert_eq!(cleanup.load(Ordering::Acquire), 1);
        let jobs = persisted_jobs(&state);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status(), &JobStatus::Cancelled);
        assert_eq!(
            jobs[0].last_outcome().map(|outcome| outcome.code.as_str()),
            Some("cancelled")
        );
        assert_eq!(reconciler.status().job_status, Some(JobStatus::Cancelled));
        assert!(matches!(
            reconciler.cancel_job(),
            Err(AcmeManagedError::NoJob)
        ));
    }
}

#[test]
fn permanent_and_retryable_protocol_failures_control_automatic_retries() {
    for (problem_type, retryable) in [("rateLimited", true), ("rejectedIdentifier", false)] {
        let temp = TempDir::new().expect("state directory");
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        let revisions = RevisionStore::from_arc(Arc::clone(&state));
        let names = vec!["proxy.example.test".to_owned()];
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed",
            &names,
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
        let reconciler = AcmeManagedReconciler::new(
            "managed",
            names.clone(),
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                challenge: AcmeChallengeType::Http01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: None,
            },
            revisions,
            "bootstrap".into(),
            None,
            None,
            true,
            ChallengeStore::default(),
            Arc::clone(&active),
        );
        let transport = FakePebbleTransport {
            responses: Arc::new(Mutex::new(VecDeque::from([
                directory_response(),
                nonce_response("nonce-account"),
                account_response(),
                problem_response(problem_type),
            ]))),
            requests: Arc::new(Mutex::new(Vec::new())),
            ca: Arc::new(test_ca().expect("CA")),
            certificate: Arc::new(Mutex::new(None)),
            certificate_names: names,
        };

        assert!(matches!(
            reconciler.renew_with_transport(transport),
            Err(AcmeManagedError::Protocol(AcmeError::Problem { .. }))
        ));
        let status = reconciler.status();
        assert_eq!(status.last_error_code.as_deref(), Some("acme_problem"));
        if retryable {
            assert_eq!(status.retry_attempt, 1);
            let next = status.next_action_unix_seconds.expect("retry schedule");
            assert!(!reconciler.renewal_due(next.saturating_sub(1)));
        } else {
            assert_eq!(status.retry_attempt, 0);
            assert_eq!(status.next_action_unix_seconds, None);
            assert!(!reconciler.renewal_due(u64::MAX));
        }
        let renewal = state
            .read_json::<PersistedRenewal>("certificates/managed/renewal.json", MAX_JOB_BYTES)
            .expect("persisted renewal state");
        assert_eq!(renewal.last_error_code.as_deref(), Some("acme_problem"));
        assert_eq!(renewal.auto_retry_blocked, !retryable);
    }
}

#[test]
fn pause_and_resume_restores_the_persisted_ari_schedule() {
    let temp = TempDir::new().expect("state directory");
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    state
        .write_json(
            "certificates/managed/renewal.json",
            &PersistedRenewal {
                certificate: "managed".into(),
                identifiers: vec!["proxy.example.test".into()],
                directory_url: "https://acme.test/directory".into(),
                account_url: Some("https://acme.test/acme/acct/1".into()),
                authenticator: "http01".into(),
                dns_provider: None,
                key_type: "ecdsa_p256".into(),
                next_action_unix_seconds: Some(4_000),
                retry_at_unix_seconds: None,
                retry_attempt: 0,
                suggested_renewal_unix_seconds: Some(4_000),
                auto_retry_blocked: false,
                renewal_info_url: Some("https://acme.test/acme/renewal-info".into()),
                last_success_unix_seconds: Some(1_000),
                last_error_code: None,
                renewal_information_status: "applied".into(),
                paused: false,
            },
        )
        .expect("renewal state");
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed",
        &["proxy.example.test".into()],
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
    let reconciler = AcmeManagedReconciler::new(
        "managed",
        vec!["proxy.example.test".into()],
        AcmeManagedPolicy {
            directory_url: "https://acme.test/directory".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::Http01,
            key_type: AcmeKeyType::EcdsaP256,
            allowed_dns_suffixes: vec!["example.test".into()],
            retained_revisions: 3,
            retention_days: 30,
            dns01: None,
        },
        RevisionStore::from_arc(Arc::clone(&state)),
        "revision".into(),
        Some(900),
        Some(10_000),
        false,
        ChallengeStore::default(),
        active,
    );

    assert_eq!(reconciler.status().next_action_unix_seconds, Some(4_000));
    reconciler.pause().expect("pause");
    assert_eq!(reconciler.status().next_action_unix_seconds, None);
    reconciler.resume().expect("resume");
    assert_eq!(reconciler.status().next_action_unix_seconds, Some(4_000));
    assert_administrative_action_job_lifecycle(&reconciler, &state);
}
