#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn local_fake_pebble_issues_validates_and_publishes_managed_certificate() {
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
        revisions.clone(),
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
            order_response("pending"),
            authorization_response("pending"),
            challenge_response(),
            authorization_response("valid"),
            order_response("processing"),
            order_response("valid"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        ca: Arc::new(test_ca().expect("CA")),
        certificate: Arc::new(Mutex::new(None)),
        certificate_names: vec!["proxy.example.test".into()],
    };
    let requests = Arc::clone(&transport.requests);
    let outcome = reconciler
        .renew_with_transport(transport)
        .expect("managed issuance");
    assert_eq!(outcome, AcmeManagedOutcome::Activated);
    assert_ne!(reconciler.status().active_revision, "bootstrap");
    let material = revisions
        .load_current("managed")
        .expect("published revision");
    assert!(material.metadata.issuer.is_some());
    assert!(material.metadata.serial_fingerprint.is_some());
    assert!(reconciler.challenge_store.is_empty());
    assert_eq!(reconciler.status().job_status, Some(JobStatus::Succeeded));
    assert!(reconciler.status().last_success_unix_seconds.is_some());
    let jobs = persisted_jobs(&state);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status(), &JobStatus::Succeeded);
    assert_eq!(jobs[0].attempt(), 1);
    assert!(jobs[0].last_outcome().is_some());
    let renewal = fs::read_to_string(temp.path().join("state/certificates/managed/renewal.json"))
        .expect("renewal schedule");
    assert!(renewal.contains("proxy.example.test"));
    assert!(
        requests
            .lock()
            .expect("request log")
            .iter()
            .all(|url| url.starts_with("https://acme.test/"))
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn local_fake_pebble_renews_activates_and_rolls_back_without_leaking_challenges() {
    let temp = TempDir::new().expect("state directory");
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    let revisions = RevisionStore::from_arc(Arc::clone(&state));
    let names = vec!["proxy.example.test".to_owned()];
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed-renewal",
        &names,
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
    let bootstrap_revision = active.snapshot().metadata().revision.clone();
    let reconciler = AcmeManagedReconciler::new(
        "managed-renewal",
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
        revisions.clone(),
        "bootstrap".into(),
        None,
        None,
        true,
        ChallengeStore::default(),
        Arc::clone(&active),
    );

    let first = FakePebbleTransport {
        responses: Arc::new(Mutex::new(VecDeque::from([
            directory_response(),
            nonce_response("nonce-first-account"),
            account_response(),
            order_response("pending"),
            authorization_response("pending"),
            challenge_response(),
            authorization_response("valid"),
            order_response("processing"),
            order_response("valid"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        ca: Arc::new(test_ca().expect("CA")),
        certificate: Arc::new(Mutex::new(None)),
        certificate_names: names.clone(),
    };
    assert_eq!(
        reconciler
            .renew_with_transport(first)
            .expect("first managed issuance"),
        AcmeManagedOutcome::Activated
    );
    let first_revision = active.snapshot().metadata().revision.clone();
    assert_ne!(first_revision, bootstrap_revision);
    assert_eq!(reconciler.status().disk_revision, first_revision);
    assert_eq!(reconciler.status().active_revision, first_revision);
    assert!(
        revisions
            .state()
            .root()
            .join(format!(
                "certificates/managed-renewal/revisions/{first_revision}"
            ))
            .is_dir()
    );
    assert!(reconciler.challenge_store.is_empty());

    let second = FakePebbleTransport {
        responses: Arc::new(Mutex::new(VecDeque::from([
            directory_response(),
            nonce_response("nonce-second-order"),
            order_response("pending"),
            authorization_response("pending"),
            challenge_response(),
            authorization_response("valid"),
            order_response("processing"),
            order_response("valid"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        ca: Arc::new(test_ca().expect("CA")),
        certificate: Arc::new(Mutex::new(None)),
        certificate_names: names.clone(),
    };
    assert_eq!(
        reconciler
            .renew_with_transport(second)
            .expect("managed renewal"),
        AcmeManagedOutcome::Activated
    );
    let second_revision = active.snapshot().metadata().revision.clone();
    assert_ne!(second_revision, first_revision);
    assert_eq!(reconciler.status().disk_revision, second_revision);
    assert_eq!(reconciler.status().active_revision, second_revision);
    assert_eq!(reconciler.status().job_status, Some(JobStatus::Succeeded));
    assert!(reconciler.challenge_store.is_empty());
    assert!(
        revisions
            .state()
            .root()
            .join(format!(
                "certificates/managed-renewal/revisions/{first_revision}"
            ))
            .is_dir()
    );

    let failed = FakePebbleTransport {
        responses: Arc::new(Mutex::new(VecDeque::from([
            directory_response(),
            nonce_response("nonce-failed-order"),
            order_response("pending"),
            authorization_response("pending"),
            challenge_response(),
            authorization_response("invalid"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        ca: Arc::new(test_ca().expect("CA")),
        certificate: Arc::new(Mutex::new(None)),
        certificate_names: names,
    };
    assert!(matches!(
        reconciler.renew_with_transport(failed),
        Err(AcmeManagedError::AuthorizationFailed)
    ));
    assert_eq!(active.snapshot().metadata().revision, second_revision);
    assert_eq!(
        revisions
            .load_current("managed-renewal")
            .unwrap()
            .metadata
            .revision,
        second_revision
    );
    assert!(reconciler.challenge_store.is_empty());
    assert_eq!(reconciler.status().job_status, Some(JobStatus::Failed));
    assert_eq!(
        reconciler.status().last_error_code.as_deref(),
        Some("authorization_failed")
    );
    let jobs = persisted_jobs(&state);
    assert_eq!(
        jobs.iter()
            .filter(|job| job.status() == &JobStatus::Succeeded)
            .count(),
        2
    );
    assert_eq!(
        jobs.iter()
            .filter(|job| job.status() == &JobStatus::Failed)
            .count(),
        1
    );
    assert!(jobs.iter().all(|job| job.attempt() == 1));
}

#[test]
fn tls_alpn01_issues_cleans_up_and_keeps_a_redacted_audit_record() {
    let temp = TempDir::new().expect("state directory");
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    let revisions = RevisionStore::from_arc(Arc::clone(&state));
    let names = vec!["proxy.example.test".to_owned()];
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed-tls-alpn",
        &names,
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
    let tls_alpn_challenge_store = TlsAlpnChallengeStore::default();
    let reconciler = AcmeManagedReconciler::new_with_challenge_stores(
        "managed-tls-alpn",
        names,
        AcmeManagedPolicy {
            directory_url: "https://acme.test/directory".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::TlsAlpn01,
            key_type: AcmeKeyType::EcdsaP256,
            allowed_dns_suffixes: vec!["example.test".into()],
            retained_revisions: 3,
            retention_days: 30,
            dns01: None,
        },
        revisions.clone(),
        "bootstrap".into(),
        None,
        None,
        true,
        ChallengeStore::default(),
        tls_alpn_challenge_store.clone(),
        None,
        Arc::clone(&active),
    );
    let transport = FakePebbleTransport {
        responses: Arc::new(Mutex::new(VecDeque::from([
            directory_response(),
            nonce_response("nonce-account"),
            account_response(),
            order_response("pending"),
            tls_alpn_authorization_response("pending"),
            challenge_response(),
            tls_alpn_authorization_response("valid"),
            order_response("processing"),
            order_response("valid"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        ca: Arc::new(test_ca().expect("CA")),
        certificate: Arc::new(Mutex::new(None)),
        certificate_names: vec!["proxy.example.test".into()],
    };
    let outcome = reconciler
        .renew_with_transport(transport)
        .expect("managed TLS-ALPN-01 issuance");
    assert_eq!(outcome, AcmeManagedOutcome::Activated);
    assert_eq!(reconciler.status().challenge, "tls_alpn01");
    assert!(tls_alpn_challenge_store.is_empty());
    assert!(revisions.load_current("managed-tls-alpn").is_ok());
    let persisted = persisted_jobs(&state);
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].status(), &JobStatus::Succeeded);

    let jobs = fs::read_dir(temp.path().join("state/jobs"))
        .expect("redacted job directory")
        .map(|entry| fs::read_to_string(entry.expect("job entry").path()).expect("job"))
        .collect::<Vec<_>>();
    assert!(!jobs.is_empty());
    assert!(jobs.iter().any(|job| job.contains("test")));
    assert!(jobs.iter().all(|job| !job.contains("token-1")));
    assert!(jobs.iter().all(|job| !job.contains("thumbprint")));
}

#[test]
fn tls_alpn01_failure_cleans_up_and_keeps_the_bootstrap_generation() {
    let temp = TempDir::new().expect("state directory");
    let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
    let revisions = RevisionStore::from_arc(Arc::clone(&state));
    let names = vec!["proxy.example.test".to_owned()];
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed-tls-alpn-failure",
        &names,
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
    let bootstrap_revision = active.snapshot().metadata().revision.clone();
    let tls_alpn_challenge_store = TlsAlpnChallengeStore::default();
    let reconciler = AcmeManagedReconciler::new_with_challenge_stores(
        "managed-tls-alpn-failure",
        names,
        AcmeManagedPolicy {
            directory_url: "https://acme.test/directory".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::TlsAlpn01,
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
        tls_alpn_challenge_store.clone(),
        None,
        Arc::clone(&active),
    );
    let transport = FakePebbleTransport {
        responses: Arc::new(Mutex::new(VecDeque::from([
            directory_response(),
            nonce_response("nonce-account"),
            account_response(),
            order_response("pending"),
            tls_alpn_authorization_response("pending"),
            challenge_response(),
            tls_alpn_authorization_response("invalid"),
        ]))),
        requests: Arc::new(Mutex::new(Vec::new())),
        ca: Arc::new(test_ca().expect("CA")),
        certificate: Arc::new(Mutex::new(None)),
        certificate_names: vec!["proxy.example.test".into()],
    };

    let error = reconciler
        .renew_with_transport(transport)
        .expect_err("invalid TLS-ALPN-01 authorization");
    assert!(matches!(error, AcmeManagedError::AuthorizationFailed));
    assert!(tls_alpn_challenge_store.is_empty());
    assert_eq!(
        reconciler
            .active_generation()
            .snapshot()
            .metadata()
            .revision,
        bootstrap_revision
    );
    assert_eq!(reconciler.status().job_status, Some(JobStatus::Failed));
    let persisted = persisted_jobs(&state);
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].status(), &JobStatus::Failed);
}
