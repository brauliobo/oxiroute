#[allow(clippy::wildcard_imports)]
use super::*;

fn due_renewal() -> PersistedRenewal {
    PersistedRenewal {
        certificate: "managed".into(),
        identifiers: vec!["proxy.example.test".into()],
        directory_url: "https://acme.test/directory".into(),
        account_url: Some("https://acme.test/acme/acct/1".into()),
        authenticator: "http01".into(),
        dns_provider: None,
        key_type: "ecdsa_p256".into(),
        next_action_unix_seconds: Some(2_000),
        retry_at_unix_seconds: None,
        retry_attempt: 0,
        suggested_renewal_unix_seconds: None,
        auto_retry_blocked: false,
        renewal_info_url: None,
        last_success_unix_seconds: Some(1_000),
        last_error_code: None,
        renewal_information_status: "not_advertised".into(),
        paused: false,
    }
}

fn reconciler_over(root: &std::path::Path) -> AcmeManagedReconciler {
    let state = Arc::new(StateStore::open(root).expect("state"));
    let bootstrap = CertificateGeneration::self_signed_development(
        "managed",
        &["proxy.example.test".into()],
        1,
        SelfSignedKeyType::EcdsaP256,
    )
    .expect("bootstrap");
    AcmeManagedReconciler::new(
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
        Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap))),
    )
}

fn seeded_root(temp: &TempDir) -> std::path::PathBuf {
    let root = temp.path().join("state");
    let owner = StateStore::open_owned(&root).expect("seed state");
    owner
        .write_json("certificates/managed/renewal.json", &due_renewal())
        .expect("renewal state");
    owner.release();
    root
}

#[test]
fn an_unclaimed_reconciler_neither_renews_nor_writes() {
    let temp = TempDir::new().expect("state directory");
    let reconciler = reconciler_over(&seeded_root(&temp));

    assert!(!reconciler.renewal_due(5_000));
    assert!(matches!(
        reconciler.pause(),
        Err(AcmeManagedError::State(AcmeStateError::NotOwned))
    ));

    reconciler.claim_state().expect("claim");
    assert!(reconciler.renewal_due(5_000));
}

#[test]
fn the_active_owner_blocks_a_claim_with_a_diagnostic_naming_the_certificate() {
    let temp = TempDir::new().expect("state directory");
    let root = seeded_root(&temp);
    let active = reconciler_over(&root);
    let candidate = reconciler_over(&root);
    active.claim_state().expect("active claims");

    let Err(error) = candidate.claim_state() else {
        panic!("candidate claimed a state root the active worker owns")
    };
    assert!(
        matches!(error, TlsBuildError::AcmeStateOwned { .. }),
        "{error}"
    );
    let message = error.to_string();
    assert!(message.contains("`managed`"), "{message}");
    assert!(
        message.contains(root.to_str().expect("UTF-8 root")),
        "{message}"
    );
    assert!(message.contains("owned by the active worker"), "{message}");
    assert!(active.renewal_due(5_000));
    assert!(!candidate.renewal_due(5_000));
}

#[test]
fn a_released_reconciler_hands_renewal_to_the_claimant_and_resumes_on_reclaim() {
    let temp = TempDir::new().expect("state directory");
    let root = seeded_root(&temp);
    let active = Arc::new(reconciler_over(&root));
    let candidate = reconciler_over(&root);
    let claims = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&claims);
    active.on_state_claimed(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    active.claim_state().expect("active claims");
    assert_eq!(claims.load(Ordering::SeqCst), 1);

    active.release_state();
    assert!(!active.renewal_due(5_000));
    candidate.claim_state().expect("claim after the release");
    assert!(candidate.renewal_due(5_000));
    assert!(!active.renewal_due(5_000));

    let reclaiming = {
        let active = Arc::clone(&active);
        std::thread::spawn(move || active.reclaim_state())
    };
    candidate.release_state();
    reclaiming
        .join()
        .expect("reclaim thread")
        .expect("reclaim after the candidate released");

    assert!(active.renewal_due(5_000));
    assert!(!candidate.renewal_due(5_000));
    assert_eq!(claims.load(Ordering::SeqCst), 2);
}
