#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn selects_exact_wildcard_and_default_certificate_generations() {
    let temp = TempDir::new().unwrap();
    let primary = write_identity(temp.path(), "primary", "www.example.test", false);
    let wildcard = write_identity(temp.path(), "wildcard", "*.example.test", false);
    let exact = write_identity(temp.path(), "exact", "api.example.test", false);
    let mut config = config_with_identity(&primary);
    config.certificates.extend([
        Certificate {
            name: "wildcard".into(),
            dns_names: vec!["*.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path: wildcard.chain,
                private_key_path: wildcard.key,
            },
        },
        Certificate {
            name: "exact".into(),
            dns_names: vec!["api.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path: exact.chain,
                private_key_path: exact.key,
            },
        },
    ]);
    config.tls_profiles[0].certificates = vec!["primary".into(), "wildcard".into(), "exact".into()];

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let profile = prepared.profiles().get("public").unwrap();
    assert_eq!(profile.default_certificate(), "primary");

    for (server_name, expected) in [
        (Some("API.EXAMPLE.TEST"), "exact"),
        (Some("blog.example.test"), "wildcard"),
        (Some(".example.test"), "primary"),
        (Some("nested.www.example.test"), "primary"),
        (Some("unknown.test"), "primary"),
        (Some("*.example.test"), "primary"),
        (Some("-invalid.example.test"), "primary"),
        (Some("invalid_.example.test"), "primary"),
        (Some("127.0.0.1"), "primary"),
        (None, "primary"),
    ] {
        assert_eq!(
            profile.selected_generation(server_name).metadata().name,
            expected
        );
    }
}

#[test]
fn rejects_missing_malformed_oversized_and_multiple_chain_material() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let missing = temp.path().join("missing.pem");
    let error = CertificateGeneration::from_files(
        "missing",
        &["www.example.test".into()],
        &missing,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::FileOpen { .. }));
    assert!(error.source().is_some());

    let malformed = temp.path().join("malformed.pem");
    fs::write(
        &malformed,
        b"-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let error = CertificateGeneration::from_files(
        "malformed",
        &["www.example.test".into()],
        &malformed,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::CertificateParse { .. }));
    assert!(error.source().is_some());

    let oversized = temp.path().join("oversized.pem");
    fs::write(&oversized, vec![b'x'; MAX_CERTIFICATE_CHAIN_BYTES + 1]).unwrap();
    let error = CertificateGeneration::from_files(
        "oversized",
        &["www.example.test".into()],
        &oversized,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::FileTooLarge { .. }));

    let too_many = temp.path().join("too-many.pem");
    let leaf_and_ca = fs::read(&files.chain).unwrap();
    let first_certificate_end = leaf_and_ca
        .windows(b"-----END CERTIFICATE-----".len())
        .position(|window| window == b"-----END CERTIFICATE-----")
        .unwrap()
        + b"-----END CERTIFICATE-----".len();
    let leaf = &leaf_and_ca[..first_certificate_end];
    let ca = fs::read(&files.ca).unwrap();
    let mut chain = leaf.to_vec();
    chain.push(b'\n');
    for _ in 0..16 {
        chain.extend_from_slice(&ca);
    }
    fs::write(&too_many, chain).unwrap();
    let error = CertificateGeneration::from_files(
        "too-many",
        &["www.example.test".into()],
        &too_many,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::TooManyChainCertificates { count: 17, .. }
    ));
}

#[test]
fn rejects_private_key_mismatch_and_redacts_key_parse_errors() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let other_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let mismatch_path = temp.path().join("mismatch-key.pem");
    write_private_key(
        &mismatch_path,
        &other_key.private_key_to_pem_pkcs8().unwrap(),
    );
    let error = CertificateGeneration::from_files(
        "mismatch",
        &["www.example.test".into()],
        &files.chain,
        &mismatch_path,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::PrivateKeyMismatch { .. }));

    let malformed_path = temp.path().join("malformed-key.pem");
    write_private_key(
        &malformed_path,
        b"-----BEGIN PRIVATE KEY-----\nSUPER_SECRET_SENTINEL\n-----END PRIVATE KEY-----\n",
    );
    let error = CertificateGeneration::from_files(
        "redacted",
        &["www.example.test".into()],
        &files.chain,
        &malformed_path,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::PrivateKeyParse { .. }));
    assert!(!format!("{error:?}").contains("SUPER_SECRET_SENTINEL"));
    assert!(!error.to_string().contains("SUPER_SECRET_SENTINEL"));
}

#[cfg(unix)]
#[test]
fn direct_file_reads_reject_final_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "nofollow", "nofollow.example.test", false);
    let outside = TempDir::new().unwrap();
    let outside_chain = outside.path().join("chain.pem");
    fs::copy(&files.chain, &outside_chain).unwrap();
    fs::remove_file(&files.chain).unwrap();
    symlink(&outside_chain, &files.chain).unwrap();

    let error = CertificateGeneration::from_files(
        "nofollow",
        &["nofollow.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::FileOpen { .. }));
}

#[test]
fn rejects_unsupported_and_weak_private_keys() {
    let temp = TempDir::new().unwrap();
    let ca_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let ca = build_certificate(
        "Key Policy Root",
        None,
        &ca_key,
        &ca_key,
        &[],
        false,
        true,
        false,
    );

    let dsa_key = PKey::from_dsa(Dsa::generate(2048).unwrap()).unwrap();
    let dsa_leaf = build_certificate(
        "dsa.example.test",
        Some(&ca),
        &ca_key,
        &dsa_key,
        &["dsa.example.test"],
        false,
        false,
        true,
    );
    let dsa_files = write_identity_material(temp.path(), "dsa", &dsa_leaf, &[&ca], &dsa_key);
    let error = CertificateGeneration::from_files(
        "dsa",
        &["dsa.example.test".into()],
        &dsa_files.chain,
        &dsa_files.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::UnsupportedPrivateKeyAlgorithm { .. }
    ));

    let weak_key = PKey::from_rsa(Rsa::generate(1024).unwrap()).unwrap();
    let weak_leaf = build_certificate(
        "weak.example.test",
        Some(&ca),
        &ca_key,
        &weak_key,
        &["weak.example.test"],
        false,
        false,
        true,
    );
    let weak_files = write_identity_material(temp.path(), "weak", &weak_leaf, &[&ca], &weak_key);
    let error = CertificateGeneration::from_files(
        "weak",
        &["weak.example.test".into()],
        &weak_files.chain,
        &weak_files.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::PrivateKeyTooWeak {
            bits: 1024,
            minimum_bits: 2048,
            ..
        }
    ));
}

#[test]
fn rejects_leaf_key_usage_without_digital_signature() {
    let temp = TempDir::new().unwrap();
    let ca_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let ca = build_certificate(
        "Key Usage Root",
        None,
        &ca_key,
        &ca_key,
        &[],
        false,
        true,
        false,
    );
    let leaf_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let leaf = build_certificate_with_leaf_usage(
        "usage.example.test",
        Some(&ca),
        &ca_key,
        &leaf_key,
        &["usage.example.test"],
        &[],
        false,
        false,
        true,
        LeafKeyUsage::KeyEncipherment,
    );
    let files = write_identity_material(temp.path(), "usage", &leaf, &[&ca], &leaf_key);

    let error = CertificateGeneration::from_files(
        "usage",
        &["usage.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::MissingDigitalSignatureKeyUsage { .. }
    ));
}

#[cfg(unix)]
#[test]
fn accepts_only_explicitly_secure_private_key_modes() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "permissions", "modes.example.test", false);
    for mode in [0o400, 0o600, 0o440, 0o640] {
        fs::set_permissions(&files.key, fs::Permissions::from_mode(mode)).unwrap();
        CertificateGeneration::from_files(
            "permissions",
            &["modes.example.test".into()],
            &files.chain,
            &files.key,
        )
        .unwrap();
    }

    for mode in [0o700, 0o660, 0o644, 0o604, 0o6400] {
        fs::set_permissions(&files.key, fs::Permissions::from_mode(mode)).unwrap();
        let error = CertificateGeneration::from_files(
            "permissions",
            &["modes.example.test".into()],
            &files.chain,
            &files.key,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            TlsBuildError::InsecurePrivateKeyPermissions { .. }
        ));
    }
}
