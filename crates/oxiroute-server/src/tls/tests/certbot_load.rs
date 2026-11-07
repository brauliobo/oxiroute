#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
#[test]
fn loads_one_certbot_revision_and_archive_private_key_reuse() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let [reconciler] = prepared.certbot_reconcilers() else {
        panic!("one Certbot reconciler must be prepared");
    };
    assert_eq!(reconciler.active_archive_revision(), 1);
    assert!(Arc::ptr_eq(
        reconciler.active_generation(),
        prepared.certificates().get("primary").unwrap()
    ));
    assert_eq!(
        prepared
            .certificates()
            .get("primary")
            .unwrap()
            .snapshot()
            .metadata()
            .dns_names,
        ["certbot.example.test"]
    );

    copy_certbot_revision(&files.archive, 1, 2, true);
    set_live_revision(&files, 2);
    let generation = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("primary", &["certbot.example.test".into()])
        .unwrap();
    assert_eq!(generation.metadata().dns_names, ["certbot.example.test"]);
}

#[cfg(unix)]
#[test]
fn prepares_only_certbot_sources_for_continuous_reconciliation() {
    let temp = TempDir::new().unwrap();
    let direct = write_identity(temp.path(), "direct", "direct.example.test", false);
    let certbot = write_certbot_lineage(temp.path(), "managed-externally", "certbot.example.test");
    let mut config = config_with_identity(&direct);
    config.certificates[0].name = "direct".into();
    config.certificates[0].dns_names = vec!["direct.example.test".into()];
    config.certificates.push(Certificate {
        name: "managed-externally".into(),
        dns_names: vec!["certbot.example.test".into()],
        source: CertificateSource::Certbot {
            live_directory_path: certbot.live,
            archive_directory_path: certbot.archive,
        },
    });
    config.tls_profiles[0].certificates = vec!["direct".into(), "managed-externally".into()];
    config.tls_profiles[0].default_certificate = "direct".into();

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();

    assert_eq!(prepared.certificates().len(), 2);
    let [reconciler] = prepared.certbot_reconcilers() else {
        panic!("only the Certbot source must have a reconciler");
    };
    assert_eq!(reconciler.status().certificate, "managed-externally");
    assert!(
        prepared
            .start_certbot_watcher(CertbotWatcherConfig::default())
            .is_ok()
    );
}

#[cfg(unix)]
#[test]
fn loads_certbot_targets_through_a_symlinked_relocated_root() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let relocated_root = temp.path().join("relocated");
    fs::create_dir(&relocated_root).unwrap();
    let files = write_certbot_lineage(&relocated_root, "primary", "certbot.example.test");
    let configured_root = temp.path().join("letsencrypt");
    symlink(&relocated_root, &configured_root).unwrap();

    copy_certbot_revision(&files.archive, 1, 2, true);
    fs::remove_file(files.archive.join("privkey2.pem")).unwrap();
    symlink(
        configured_root.join("archive/primary").join("privkey1.pem"),
        files.archive.join("privkey2.pem"),
    )
    .unwrap();
    for stem in ["cert", "chain", "fullchain", "privkey"] {
        set_raw_live_link(
            &files.live,
            &format!("{stem}.pem"),
            &configured_root
                .join("archive/primary")
                .join(format!("{stem}2.pem")),
        );
    }

    let generation = CertbotLineage::new(
        configured_root.join("live/primary"),
        configured_root.join("archive/primary"),
    )
    .load("primary", &["certbot.example.test".into()])
    .unwrap();
    assert_eq!(generation.metadata().dns_names, ["certbot.example.test"]);
}

#[cfg(unix)]
#[test]
fn rejects_missing_non_symlink_and_mixed_certbot_live_entries() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let lineage = || CertbotLineage::new(files.live.clone(), files.archive.clone());

    fs::remove_file(files.live.join("cert.pem")).unwrap();
    let error = lineage()
        .load("primary", &["certbot.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::CertbotLiveLinkMetadata { .. }
    ));

    set_live_revision(&files, 1);
    fs::remove_file(files.live.join("chain.pem")).unwrap();
    fs::write(files.live.join("chain.pem"), b"not a symlink").unwrap();
    let error = lineage()
        .load("primary", &["certbot.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::CertbotLiveEntryNotSymlink { .. }
    ));

    set_live_revision(&files, 1);
    copy_certbot_revision(&files.archive, 1, 2, false);
    set_live_link(&files, "chain.pem", "chain2.pem");
    let error = lineage()
        .load("primary", &["certbot.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::MixedCertbotArchiveRevisions { .. }
    ));
}

#[cfg(unix)]
#[test]
fn rejects_invalid_or_escaping_certbot_live_targets() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");

    for target in ["cert0.pem", "cert01.pem", "cert-1.pem", "chain1.pem"] {
        set_live_link(&files, "cert.pem", target);
        let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
            .load("primary", &["certbot.example.test".into()])
            .unwrap_err();
        assert!(matches!(
            error,
            TlsBuildError::InvalidCertbotLiveLinkTarget { .. }
        ));
    }

    set_raw_live_link(
        &files.live,
        "cert.pem",
        Path::new("../../../outside/cert1.pem"),
    );
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("primary", &["certbot.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InvalidCertbotLiveLinkTarget { .. }
    ));

    let outside_archive = temp.path().join("outside-archive");
    fs::create_dir(&outside_archive).unwrap();
    fs::copy(
        files.archive.join("cert1.pem"),
        outside_archive.join("cert1.pem"),
    )
    .unwrap();
    set_raw_live_link(&files.live, "cert.pem", &outside_archive.join("cert1.pem"));
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("primary", &["certbot.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InvalidCertbotLiveLinkTarget { .. }
    ));
}

#[cfg(unix)]
#[test]
fn rejects_archive_escapes_and_non_key_archive_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");

    fs::remove_file(files.archive.join("cert1.pem")).unwrap();
    symlink("chain1.pem", files.archive.join("cert1.pem")).unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("primary", &["certbot.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::CertbotArchiveEntryNotRegular { .. }
    ));

    let files = write_certbot_lineage(temp.path(), "escaping", "escape.example.test");
    let outside_key = temp.path().join("outside-key.pem");
    fs::copy(files.archive.join("privkey1.pem"), &outside_key).unwrap();
    fs::remove_file(files.archive.join("privkey1.pem")).unwrap();
    symlink("../../outside-key.pem", files.archive.join("privkey1.pem")).unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("escaping", &["escape.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InvalidCertbotArchivePrivateKeyLink { .. }
    ));
}

#[cfg(unix)]
#[test]
fn rejects_missing_invalid_and_oversized_certbot_archive_material() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let dns_names = ["certbot.example.test".into()];

    fs::remove_file(files.archive.join("chain1.pem")).unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("primary", &dns_names)
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::CertbotArchiveEntryMetadata { .. }
    ));

    let files = write_certbot_lineage(temp.path(), "fullchain", "fullchain.example.test");
    fs::write(
        files.archive.join("fullchain1.pem"),
        fs::read(files.archive.join("cert1.pem")).unwrap(),
    )
    .unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("fullchain", &["fullchain.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::CertbotFullchainMismatch { .. }
    ));

    let files = write_certbot_lineage(temp.path(), "large-cert", "large-cert.example.test");
    fs::write(
        files.archive.join("cert1.pem"),
        vec![b'x'; MAX_CERTIFICATE_CHAIN_BYTES + 1],
    )
    .unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("large-cert", &["large-cert.example.test".into()])
        .unwrap_err();
    assert!(matches!(error, TlsBuildError::FileTooLarge { .. }));

    let files = write_certbot_lineage(temp.path(), "large-key", "large-key.example.test");
    write_private_key(
        &files.archive.join("privkey1.pem"),
        &vec![b'x'; MAX_PRIVATE_KEY_BYTES + 1],
    );
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("large-key", &["large-key.example.test".into()])
        .unwrap_err();
    assert!(matches!(error, TlsBuildError::FileTooLarge { .. }));
}

#[cfg(unix)]
#[test]
fn enforces_certbot_certificate_and_chain_pem_artifact_shapes() {
    const NON_CERTIFICATE_PEM: &[u8] =
        b"-----BEGIN PRIVATE KEY-----\nAA==\n-----END PRIVATE KEY-----\n";

    let temp = TempDir::new().unwrap();

    let files = write_certbot_lineage(temp.path(), "multi-cert", "multi-cert.example.test");
    let mut cert = fs::read(files.archive.join("cert1.pem")).unwrap();
    let chain = fs::read(files.archive.join("chain1.pem")).unwrap();
    cert.extend_from_slice(&chain);
    let mut fullchain = cert.clone();
    fullchain.extend_from_slice(&chain);
    fs::write(files.archive.join("cert1.pem"), cert).unwrap();
    fs::write(files.archive.join("fullchain1.pem"), fullchain).unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("multi-cert", &["multi-cert.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InvalidPem {
            kind: "Certbot certificate",
            detail: "cert.pem must contain exactly one CERTIFICATE block",
            ..
        }
    ));

    let files = write_certbot_lineage(temp.path(), "empty-chain", "empty-chain.example.test");
    let cert = fs::read(files.archive.join("cert1.pem")).unwrap();
    fs::write(files.archive.join("chain1.pem"), []).unwrap();
    fs::write(files.archive.join("fullchain1.pem"), cert).unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("empty-chain", &["empty-chain.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::EmptyFile {
            kind: "Certbot chain",
            ..
        }
    ));

    let files = write_certbot_lineage(temp.path(), "blank-chain", "blank-chain.example.test");
    let mut fullchain = fs::read(files.archive.join("cert1.pem")).unwrap();
    fullchain.push(b'\n');
    fs::write(files.archive.join("chain1.pem"), b"\n").unwrap();
    fs::write(files.archive.join("fullchain1.pem"), fullchain).unwrap();
    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("blank-chain", &["blank-chain.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InvalidPem {
            kind: "Certbot chain",
            detail: "no PEM blocks",
            ..
        }
    ));

    for (name, artifact, kind, detail) in [
        (
            "non-cert-cert",
            "cert",
            "Certbot certificate",
            "cert.pem must contain exactly one CERTIFICATE block",
        ),
        (
            "non-cert-chain",
            "chain",
            "Certbot chain",
            "chain.pem must contain one or more CERTIFICATE blocks only",
        ),
    ] {
        let dns_name = format!("{name}.example.test");
        let files = write_certbot_lineage(temp.path(), name, &dns_name);
        fs::write(
            files.archive.join(format!("{artifact}1.pem")),
            NON_CERTIFICATE_PEM,
        )
        .unwrap();
        let cert = fs::read(files.archive.join("cert1.pem")).unwrap();
        let chain = fs::read(files.archive.join("chain1.pem")).unwrap();
        let fullchain = [cert, chain].concat();
        fs::write(files.archive.join("fullchain1.pem"), fullchain).unwrap();

        let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
            .load(name, &[dns_name])
            .unwrap_err();
        assert!(matches!(
            error,
            TlsBuildError::InvalidPem {
                kind: actual_kind,
                detail: actual_detail,
                ..
            } if actual_kind == kind && actual_detail == detail
        ));
    }
}

#[cfg(unix)]
#[test]
fn enforces_secure_mode_on_the_resolved_certbot_private_key() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    fs::set_permissions(
        files.archive.join("privkey1.pem"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    let error = CertbotLineage::new(files.live.clone(), files.archive.clone())
        .load("primary", &["certbot.example.test".into()])
        .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InsecurePrivateKeyPermissions { .. }
    ));
}

#[cfg(unix)]
#[test]
fn exposes_a_redacted_certbot_candidate_with_its_archive_revision() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");

    let candidate = CertbotLineage::new(files.live, files.archive)
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let _: &CertbotCandidate = &candidate;

    assert_eq!(candidate.archive_revision(), 1);
    assert_eq!(candidate.generation().metadata().name, "primary");
    let debug = format!("{candidate:?}");
    assert!(debug.contains("archive_revision"));
    assert!(!debug.contains("PRIVATE KEY"));
    assert!(!debug.contains("private_key"));
}
