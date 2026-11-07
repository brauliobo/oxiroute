use super::*;

fn material() -> CertificateMaterial {
    CertificateMaterial {
        certificate_pem: b"cert".to_vec(),
        chain_pem: b"chain".to_vec(),
        fullchain_pem: b"fullchain".to_vec(),
        private_key_pem: SecretBytes::new(b"private".to_vec()),
        metadata: RevisionMetadata {
            certificate: "edge".into(),
            revision: "0123abcd".into(),
            created_at_unix_seconds: 10,
            not_before_unix_seconds: None,
            not_after_unix_seconds: None,
            issuer: None,
            serial_fingerprint: None,
            key_type: Some("ecdsa_p256".into()),
        },
    }
}

#[test]
fn state_root_lock_and_revision_are_secure_and_redacted() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = StateStore::open(directory.path().join("state")).expect("state");
    let revision = RevisionStore::new(state);
    let material = material();
    revision
        .commit("edge", "0123abcd", &material)
        .expect("commit");

    let loaded = revision.load_current("edge").expect("load");
    assert_eq!(loaded.certificate_pem, material.certificate_pem);
    assert_eq!(loaded.private_key_pem.as_bytes(), b"private");
    let debug = format!("{loaded:?}");
    assert!(debug.contains("SecretBytes(REDACTED)"));
    assert!(!debug.contains("private\""));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let root_mode = fs::metadata(directory.path().join("state"))
            .expect("root metadata")
            .permissions()
            .mode()
            & 0o777;
        let key_mode = fs::metadata(
            directory
                .path()
                .join("state/certificates/edge/revisions/0123abcd/privkey.pem"),
        )
        .expect("key metadata")
        .permissions()
        .mode()
            & 0o777;
        assert_eq!(root_mode, 0o700);
        assert_eq!(key_mode, 0o600);
    }
}

#[test]
fn same_process_state_root_reuses_the_lock_for_generation_reload() {
    let directory = tempfile::tempdir().expect("tempdir");
    let first = StateStore::open(directory.path().join("state")).expect("first state");
    let second = StateStore::open(directory.path().join("state")).expect("second state");
    assert_eq!(first.root(), second.root());
}

#[test]
fn old_revision_survives_current_pointer_replacement() {
    let directory = tempfile::tempdir().expect("tempdir");
    let revision =
        RevisionStore::new(StateStore::open(directory.path().join("state")).expect("state"));
    let first = material();
    revision.commit("edge", "00000001", &first).expect("first");
    let mut second = material();
    second.certificate_pem = b"new-cert".to_vec();
    revision
        .commit("edge", "00000002", &second)
        .expect("second");

    assert_eq!(
        revision
            .load_current("edge")
            .expect("current")
            .certificate_pem,
        b"new-cert"
    );
    assert!(
        directory
            .path()
            .join("state/certificates/edge/revisions/00000001/cert.pem")
            .is_file()
    );
}

#[test]
fn garbage_collection_preserves_current_and_newest_revisions() {
    let directory = tempfile::tempdir().expect("tempdir");
    let revision =
        RevisionStore::new(StateStore::open(directory.path().join("state")).expect("state"));
    for (revision_id, created_at) in [("00000001", 1), ("00000002", 2), ("00000003", 3)] {
        let mut material = material();
        material.metadata.revision = revision_id.into();
        material.metadata.created_at_unix_seconds = created_at;
        revision
            .commit("edge", revision_id, &material)
            .expect("revision");
    }
    assert_eq!(revision.garbage_collect("edge", 1, 3).expect("collect"), 2);
    assert!(
        !directory
            .path()
            .join("state/certificates/edge/revisions/00000001")
            .exists()
    );
    assert!(
        directory
            .path()
            .join("state/certificates/edge/revisions/00000003")
            .is_dir()
    );
    assert_eq!(
        revision
            .load_current("edge")
            .expect("active revision")
            .metadata
            .revision,
        "00000003"
    );
}

#[test]
fn deleting_certificate_state_removes_account_and_revisions() {
    let directory = tempfile::tempdir().expect("tempdir");
    let revision =
        RevisionStore::new(StateStore::open(directory.path().join("state")).expect("state"));
    let material = material();
    revision
        .commit("edge", "0123abcd", &material)
        .expect("revision");
    revision
        .state()
        .write_secret(
            "accounts/edge/account-key.pem",
            &SecretBytes::new(b"key".to_vec()),
        )
        .expect("account key");
    revision
        .state()
        .write_public("accounts/edge/account.json", b"{}")
        .expect("account state");
    revision
        .delete_certificate_state("edge")
        .expect("delete state");
    assert!(!directory.path().join("state/certificates/edge").exists());
    assert!(!directory.path().join("state/accounts/edge").exists());
}

#[test]
fn deleting_certificate_state_blocks_unresolved_dns_cleanup() {
    let directory = tempfile::tempdir().expect("tempdir");
    let revision =
        RevisionStore::new(StateStore::open(directory.path().join("state")).expect("state"));
    revision
        .commit("edge", "0123abcd", &material())
        .expect("revision");
    revision
        .state()
        .write_secret(
            "certificates/edge/dns-cleanup.json",
            &SecretBytes::new(b"pending-cleanup".to_vec()),
        )
        .expect("cleanup journal");

    assert!(matches!(
        revision.delete_certificate_state("edge"),
        Err(AcmeStateError::PendingDnsCleanup)
    ));
    assert!(directory.path().join("state/certificates/edge").is_dir());
    assert!(
        directory
            .path()
            .join("state/certificates/edge/dns-cleanup.json")
            .is_file()
    );
}

#[cfg(unix)]
#[test]
fn deleting_certificate_state_rejects_a_symlinked_revision_directory() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("tempdir");
    let revision =
        RevisionStore::new(StateStore::open(directory.path().join("state")).expect("state"));
    revision
        .commit("edge", "0123abcd", &material())
        .expect("revision");
    let external = directory.path().join("external");
    fs::create_dir(&external).expect("external directory");
    let revisions = directory.path().join("state/certificates/edge/revisions");
    fs::remove_dir_all(&revisions).expect("remove revisions");
    symlink(&external, &revisions).expect("symlink revisions");

    assert!(matches!(
        revision.delete_certificate_state("edge"),
        Err(AcmeStateError::UnsafeDirectory)
    ));
    assert!(external.is_dir());
}

#[test]
fn job_state_is_durable_and_contains_only_redacted_outcomes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = StateStore::open(directory.path().join("state")).expect("state");
    state
        .create_job("job-1", "edge", "renew", None, 1)
        .expect("create job");
    state.start_job("job-1", 2).expect("start job");
    let job = state
        .fail_job(
            "job-1",
            3,
            Some(4),
            None,
            Some("old".into()),
            RedactedOutcome::new("transport_failed", "retry scheduled"),
        )
        .expect("fail job");
    assert_eq!(state.read_job("job-1").expect("read job"), job);
    assert_eq!(job.id(), "job-1");
    assert_eq!(job.certificate(), "edge");
    assert_eq!(job.operation(), "renew");
    assert_eq!(job.correlation_id(), None);
    assert_eq!(job.status(), &JobStatus::Failed);
    assert_eq!(job.created_at_unix_seconds(), 1);
    assert_eq!(job.updated_at_unix_seconds(), 3);
    assert_eq!(job.attempt(), 1);
    assert_eq!(job.next_action_unix_seconds(), Some(4));
    assert_eq!(job.disk_revision(), None);
    assert_eq!(job.active_revision(), Some("old"));
    assert_eq!(
        job.last_outcome().map(|outcome| outcome.code.as_str()),
        Some("transport_failed")
    );
    let json =
        fs::read_to_string(directory.path().join("state/jobs/job-1.json")).expect("job JSON");
    assert!(json.contains(r#""status":"failed""#));
    assert!(json.contains(r#""correlationId":null"#));
    assert!(json.contains(r#""createdAtUnixSeconds":1"#));
    assert!(json.contains(r#""updatedAtUnixSeconds":3"#));
    assert!(json.contains(r#""nextActionUnixSeconds":4"#));
    assert!(!json.contains("private"));
    assert!(!json.contains("token"));
}

#[test]
fn job_transition_matrix_is_explicit_and_terminal_states_are_terminal() {
    let statuses = [
        JobStatus::Queued,
        JobStatus::Running,
        JobStatus::WaitingForChallenge,
        JobStatus::Finalizing,
        JobStatus::Paused,
        JobStatus::Succeeded,
        JobStatus::Failed,
        JobStatus::Cancelled,
    ];
    let legal = [
        (JobStatus::Queued, JobStatus::Running),
        (JobStatus::Running, JobStatus::WaitingForChallenge),
        (JobStatus::Running, JobStatus::Finalizing),
        (JobStatus::Running, JobStatus::Succeeded),
        (JobStatus::Running, JobStatus::Failed),
        (JobStatus::Running, JobStatus::Cancelled),
        (JobStatus::Running, JobStatus::Paused),
        (JobStatus::WaitingForChallenge, JobStatus::Finalizing),
        (JobStatus::WaitingForChallenge, JobStatus::Failed),
        (JobStatus::WaitingForChallenge, JobStatus::Cancelled),
        (JobStatus::WaitingForChallenge, JobStatus::Paused),
        (JobStatus::Finalizing, JobStatus::Succeeded),
        (JobStatus::Finalizing, JobStatus::Failed),
        (JobStatus::Finalizing, JobStatus::Cancelled),
        (JobStatus::Finalizing, JobStatus::Paused),
    ];

    for current in &statuses {
        for next in &statuses {
            assert_eq!(
                legal_job_transition(current, next),
                legal.contains(&(current.clone(), next.clone())),
                "{current:?} -> {next:?}"
            );
        }
    }
}

#[test]
fn every_store_emitted_status_satisfies_job_invariants() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = StateStore::open(directory.path().join("state")).expect("state");
    let outcome = || RedactedOutcome::new("complete", "redacted outcome");

    let queued = state
        .create_job("queued", "edge", "renew", Some("trace:1".into()), 1)
        .expect("queued");
    assert!(is_valid_emitted_job(&queued));
    let running = state.start_job("queued", 2).expect("running");
    assert!(is_valid_emitted_job(&running));
    let waiting = state.wait_job("queued", 3).expect("waiting");
    assert!(is_valid_emitted_job(&waiting));
    let finalizing = state.finalize_job("queued", 4).expect("finalizing");
    assert!(is_valid_emitted_job(&finalizing));
    let succeeded = state
        .succeed_job(
            "queued",
            5,
            Some("disk".into()),
            Some("active".into()),
            outcome(),
        )
        .expect("succeeded");
    assert!(is_valid_emitted_job(&succeeded));
    assert!(matches!(
        state.fail_job("queued", 6, None, None, None, outcome()),
        Err(AcmeStateError::InvalidJobTransition)
    ));

    for (id, expected) in [
        ("failed", JobStatus::Failed),
        ("cancelled", JobStatus::Cancelled),
        ("paused", JobStatus::Paused),
        ("action", JobStatus::Succeeded),
    ] {
        state
            .create_job(id, "edge", "renew", None, 10)
            .expect("create terminal job");
        state.start_job(id, 11).expect("start terminal job");
        let terminal = match expected {
            JobStatus::Failed => state
                .fail_job(id, 12, Some(13), None, None, outcome())
                .expect("failed"),
            JobStatus::Cancelled => state
                .cancel_job(id, 12, None, None, outcome())
                .expect("cancelled"),
            JobStatus::Paused => state
                .pause_job(id, 12, None, None, outcome())
                .expect("paused"),
            JobStatus::Succeeded => state
                .succeed_job(id, 12, None, None, outcome())
                .expect("action succeeded"),
            _ => unreachable!(),
        };
        assert_eq!(terminal.status(), &expected);
        assert!(is_valid_emitted_job(&terminal));
    }
}

#[test]
fn historical_json_recovers_every_status_without_enforcing_new_write_invariants() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = StateStore::open(directory.path().join("state")).expect("state");
    for (index, (spelling, status)) in [
        ("queued", JobStatus::Queued),
        ("running", JobStatus::Running),
        ("waiting_for_challenge", JobStatus::WaitingForChallenge),
        ("finalizing", JobStatus::Finalizing),
        ("paused", JobStatus::Paused),
        ("succeeded", JobStatus::Succeeded),
        ("failed", JobStatus::Failed),
        ("cancelled", JobStatus::Cancelled),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("historical-{index}");
        let json = format!(
            r#"{{"id":"{id}","certificate":"edge","operation":"renew","status":"{spelling}","createdAtUnixSeconds":20,"updatedAtUnixSeconds":10,"attempt":999,"nextActionUnixSeconds":1,"diskRevision":"","activeRevision":null,"lastOutcome":null}}"#
        );
        state
            .write_public(&format!("jobs/{id}.json"), json.as_bytes())
            .expect("historical JSON");
        let recovered = state.read_job(&id).expect("recover historical job");
        assert_eq!(recovered.status(), &status);
        assert_eq!(recovered.correlation_id(), None);
        assert_eq!(recovered.attempt(), 999);
        assert!(!is_valid_emitted_job(&recovered));
    }
}

#[test]
fn job_timestamps_are_monotonic_created_time_is_immutable_and_attempts_are_bounded() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = StateStore::open(directory.path().join("state")).expect("state");
    state
        .create_job("clock", "edge", "renew", None, 10)
        .expect("create");
    assert!(matches!(
        state.start_job("clock", 9),
        Err(AcmeStateError::InvalidJobTransition)
    ));
    let running = state.start_job("clock", 11).expect("start");
    assert_eq!(running.created_at_unix_seconds(), 10);
    assert_eq!(running.updated_at_unix_seconds(), 11);
    assert_eq!(running.attempt(), 1);
    assert!(matches!(
        state.finalize_job("clock", 10),
        Err(AcmeStateError::InvalidJobTransition)
    ));

    let bounded = format!(
        r#"{{"id":"bounded","certificate":"edge","operation":"renew","status":"queued","createdAtUnixSeconds":1,"updatedAtUnixSeconds":1,"attempt":{MAX_JOB_ATTEMPTS},"nextActionUnixSeconds":null,"diskRevision":null,"activeRevision":null,"lastOutcome":null}}"#
    );
    state
        .write_public("jobs/bounded.json", bounded.as_bytes())
        .expect("historical bounded job");
    assert!(matches!(
        state.start_job("bounded", 2),
        Err(AcmeStateError::InvalidJobTransition)
    ));
}

#[test]
fn unsafe_paths_and_invalid_revisions_are_rejected() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = StateStore::open(directory.path().join("state")).expect("state");
    assert!(matches!(
        state.write_public("../escape", b"x"),
        Err(AcmeStateError::UnsafePath)
    ));
    assert!(matches!(
        RevisionStore::new(state).commit("edge", "not-hex", &material()),
        Err(AcmeStateError::InvalidRevision)
    ));
}
