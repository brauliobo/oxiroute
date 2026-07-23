#![allow(dead_code, unused_imports)]

#[path = "mod.rs"]
mod tls;

use std::{
    error::Error as _,
    fs,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use openssl::{
    asn1::{Asn1Integer, Asn1Time},
    bn::BigNum,
    dsa::Dsa,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    rsa::Rsa,
    ssl::{Ssl, SslContextBuilder, SslMethod, SslOptions, SslSessionCacheMode},
    x509::{
        X509, X509NameBuilder,
        extension::{
            AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
            SubjectAlternativeName, SubjectKeyIdentifier,
        },
    },
};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, HttpVersion, HttpVersionPolicy,
    TlsProfile, TlsVersion, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool, UpstreamTls,
};
use pingora::{listeners::ALPN, upstreams::peer::HttpPeer};
use tempfile::TempDir;

use tls::{
    ActiveCertificateGeneration, CertbotActivationDirection, CertbotCandidate, CertbotLineage,
    CertbotReconcileError, CertbotReconcileOutcome, CertbotReconciler, CertbotReconcilerStatus,
    CertbotWatcherConfig, CertbotWatcherError, CertbotWatcherMonitor, CertbotWatcherSupervisor,
    CertificateGeneration, CertificateMetadata, CertificatePublishError, CertificateValidity,
    MAX_CERTIFICATE_CHAIN_BYTES, MAX_PRIVATE_KEY_BYTES, TlsBuildError, TlsProfilePlan,
    UpstreamTlsPlan, prepare_tls, prepare_upstream_tls,
};

struct IdentityFiles {
    chain: PathBuf,
    key: PathBuf,
    ca: PathBuf,
}

#[test]
fn prepares_metadata_redacted_generation_and_callback_settings() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let config = config_with_identity(&files);

    let prepared = prepare_tls(&config).unwrap();
    let active_identity = prepared.certificates().get("primary").unwrap();
    let generation = active_identity.snapshot();
    let metadata = generation.metadata();
    let _: &CertificateMetadata = metadata;
    let _: &CertificateValidity = &metadata.validity;
    assert_eq!(metadata.name, "primary");
    assert_eq!(metadata.dns_names, ["www.example.test"]);
    assert_eq!(metadata.fingerprint_sha256.len(), 64);
    assert_eq!(metadata.revision.len(), 64);
    assert_eq!(metadata.intermediate_count, 1);
    assert!(!metadata.validity.not_before.is_empty());
    assert!(!metadata.validity.not_after.is_empty());

    let debug = format!("{generation:?}");
    assert!(!debug.contains("PRIVATE KEY"));
    assert!(!debug.contains("private_key"));

    let profile = prepared.profiles().get("public").unwrap();
    let _: &TlsProfilePlan = profile;
    assert_eq!(profile.name(), "public");
    assert_eq!(profile.min_version(), TlsVersion::Tls12);
    assert_eq!(profile.alpn(), &ALPN::H2H1);
    assert!(!profile.is_h2_only());
    assert!(Arc::ptr_eq(
        active_identity,
        profile.active_generation("primary").unwrap()
    ));
    assert_eq!(
        profile
            .active_generation("primary")
            .unwrap()
            .snapshot()
            .metadata()
            .revision,
        metadata.revision
    );
    let mut settings = profile.tls_settings().unwrap();
    assert!(settings.options().contains(SslOptions::NO_TICKET));
    assert_eq!(
        settings.set_session_cache_mode(SslSessionCacheMode::SERVER),
        SslSessionCacheMode::OFF
    );
}

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

    let prepared = prepare_tls(&config).unwrap();
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

#[cfg(unix)]
#[test]
fn loads_one_certbot_revision_and_archive_private_key_reuse() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);

    let prepared = prepare_tls(&config).unwrap();
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

    let prepared = prepare_tls(&config).unwrap();

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

#[cfg(target_os = "linux")]
#[test]
fn production_watcher_reconciles_the_preparation_to_startup_change_immediately() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    write_new_certbot_revision(temp.path(), &files, 2, "startup", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);
    let prepared = prepare_tls(&config).unwrap();
    let [reconciler] = prepared.certbot_reconcilers() else {
        panic!("one Certbot reconciler must be prepared");
    };

    // This activation happens after preparation and before the notify backend is installed, so it
    // cannot produce an event for the watcher. Startup reconciliation must close that gap.
    set_live_revision(&files, 2);
    let mut supervisor = prepared
        .start_certbot_watcher(CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(30),
            event_debounce: std::time::Duration::from_millis(10),
            event_max_delay: std::time::Duration::from_millis(50),
        })
        .unwrap()
        .expect("configured production watcher");

    wait_for_condition("startup reconciliation", || {
        reconciler.active_archive_revision() == 2
    });
    assert_eq!(supervisor.status().periodic_rescans, 0);
    assert!(supervisor.status().rescans > 0);
    supervisor.shutdown();
}

#[cfg(target_os = "linux")]
#[test]
fn production_watcher_reports_reconciliation_degradation_and_filesystem_recovery() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);
    let prepared = prepare_tls(&config).unwrap();
    let [reconciler] = prepared.certbot_reconcilers() else {
        panic!("one Certbot reconciler must be prepared");
    };
    let fullchain_path = files.archive.join("fullchain1.pem");
    let valid_fullchain = fs::read(&fullchain_path).unwrap();
    let mut supervisor = prepared
        .start_certbot_watcher(CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(30),
            event_debounce: std::time::Duration::from_millis(10),
            event_max_delay: std::time::Duration::from_millis(50),
        })
        .unwrap()
        .expect("configured production watcher");
    wait_for_condition("initial watcher reconciliation", || {
        supervisor.status().rescans > 0
    });

    fs::write(&fullchain_path, b"invalid replacement").unwrap();
    wait_for_condition("degraded reconciliation status", || {
        reconciler.status().last_error_code == Some("invalid_candidate")
            && supervisor.status().degraded
    });
    assert!(supervisor.status().reconciliation_failures > 0);
    assert_eq!(reconciler.active_archive_revision(), 1);

    fs::write(&fullchain_path, valid_fullchain).unwrap();
    wait_for_condition("reconciliation recovery", || {
        reconciler.status().last_error_code.is_none() && !supervisor.status().degraded
    });
    assert_eq!(reconciler.status().last_outcome, Some("unchanged"));
    assert_eq!(reconciler.active_archive_revision(), 1);
    supervisor.shutdown();
}

#[cfg(unix)]
#[test]
fn watcher_start_rejects_a_lineage_path_replaced_by_a_regular_file() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);
    let prepared = prepare_tls(&config).unwrap();

    fs::remove_dir_all(&files.live).unwrap();
    fs::write(&files.live, b"not a lineage directory").unwrap();
    let error = prepared
        .start_certbot_watcher(CertbotWatcherConfig::default())
        .unwrap_err();

    assert!(matches!(
        error,
        CertbotWatcherError::PathNotDirectory { path } if path == files.live
    ));
}

#[test]
fn optional_certbot_watcher_does_not_start_without_identities() {
    assert!(
        CertbotWatcherSupervisor::start_if_configured(Vec::new(), CertbotWatcherConfig::default())
            .unwrap()
            .is_none()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_notify_ignores_loader_access_events_and_stable_lineage_settles() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = Arc::new(CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        active,
    ));
    let mut supervisor = CertbotWatcherSupervisor::start(
        vec![reconciler],
        CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(5),
            event_debounce: std::time::Duration::from_millis(25),
            event_max_delay: std::time::Duration::from_millis(150),
        },
    )
    .unwrap();

    set_live_revision(&files, 1);
    wait_for_condition("initial notify reconciliation", || {
        supervisor.status().rescans > 0
    });
    let mut settled_rescans = supervisor.status().rescans;
    let mut quiet_since = std::time::Instant::now();
    while quiet_since.elapsed() < std::time::Duration::from_millis(250) {
        let rescans = supervisor.status().rescans;
        if rescans != settled_rescans {
            settled_rescans = rescans;
            quiet_since = std::time::Instant::now();
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(supervisor.status().ignored_access_events > 0);

    thread::sleep(std::time::Duration::from_millis(250));
    assert_eq!(supervisor.status().rescans, settled_rescans);
    supervisor.shutdown();
}

#[cfg(target_os = "linux")]
#[test]
fn linux_notify_rebuilds_watches_after_lineage_directory_replacement() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
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
            rescan_interval: std::time::Duration::from_secs(5),
            event_debounce: std::time::Duration::from_millis(25),
            event_max_delay: std::time::Duration::from_millis(150),
        },
    )
    .unwrap();

    let replacement = CertbotTestLineage {
        name: "primary".into(),
        live: temp.path().join("live/primary-replacement"),
        archive: temp.path().join("archive/primary-replacement"),
    };
    fs::create_dir(&replacement.live).unwrap();
    fs::create_dir(&replacement.archive).unwrap();
    write_new_certbot_revision(
        temp.path(),
        &replacement,
        1,
        "replacement-one",
        "certbot.example.test",
    );
    write_new_certbot_revision(
        temp.path(),
        &replacement,
        2,
        "replacement-two",
        "certbot.example.test",
    );
    set_live_revision(&replacement, 2);

    fs::rename(&files.archive, temp.path().join("archive/primary-old")).unwrap();
    fs::rename(&replacement.archive, &files.archive).unwrap();
    fs::rename(&files.live, temp.path().join("live/primary-old")).unwrap();
    fs::rename(&replacement.live, &files.live).unwrap();

    wait_for_condition("replacement revision activation", || {
        reconciler.active_archive_revision() == 2
    });
    // Republish the same links and wait for a later refresh before testing the rebuilt watch.
    let watch_refreshes_before_barrier = supervisor.status().watch_refreshes;
    set_live_revision(&files, 2);
    wait_for_condition("watch rebuild after directory replacement", || {
        reconciler.active_archive_revision() == 2
            && supervisor.status().watch_refreshes > watch_refreshes_before_barrier
    });
    assert!(!supervisor.status().degraded);
    let replacement_two = active.snapshot();
    let rescans_after_replacement = supervisor.status().rescans;

    set_live_revision(&files, 1);
    wait_for_condition("event-driven rollback after watch rebuild", || {
        reconciler.active_archive_revision() == 1
            && supervisor.status().rescans > rescans_after_replacement
    });
    assert!(!Arc::ptr_eq(&active.snapshot(), &replacement_two));
    assert!(!supervisor.status().degraded);
    supervisor.shutdown();
}

#[test]
fn rejects_expired_leaf_certificate() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "expired", "expired.example.test", true);
    let error = CertificateGeneration::from_files(
        "expired",
        &["expired.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::CertificateExpired { .. }));
}

#[test]
fn requires_declared_dns_names_to_exactly_match_valid_dns_sans() {
    let temp = TempDir::new().unwrap();
    let ca_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let ca = build_certificate("SAN Root", None, &ca_key, &ca_key, &[], false, true, false);

    let leaf_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let leaf = build_certificate(
        "ignored-cn.example.test",
        Some(&ca),
        &ca_key,
        &leaf_key,
        &["WWW.EXAMPLE.TEST", "*.EXAMPLE.TEST"],
        false,
        false,
        true,
    );
    let files = write_identity_material(temp.path(), "sans", &leaf, &[&ca], &leaf_key);
    let generation = CertificateGeneration::from_files(
        "sans",
        &["*.example.test".into(), "www.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap();
    assert_eq!(
        generation.metadata().dns_names,
        ["*.example.test", "www.example.test"]
    );

    let error = CertificateGeneration::from_files(
        "missing-declaration",
        &["www.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::DnsSanMismatch { .. }));

    let no_san_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let no_san = build_certificate(
        "www.example.test",
        Some(&ca),
        &ca_key,
        &no_san_key,
        &[],
        false,
        false,
        true,
    );
    let no_san_files = write_identity_material(temp.path(), "no-san", &no_san, &[&ca], &no_san_key);
    let error = CertificateGeneration::from_files(
        "no-cn-fallback",
        &["www.example.test".into()],
        &no_san_files.chain,
        &no_san_files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::MissingDnsSan { .. }));

    let invalid_san_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let invalid_san = build_certificate(
        "invalid.example.test",
        Some(&ca),
        &ca_key,
        &invalid_san_key,
        &["caf\u{e9}.example.test"],
        false,
        false,
        true,
    );
    let invalid_san_files = write_identity_material(
        temp.path(),
        "invalid-san",
        &invalid_san,
        &[&ca],
        &invalid_san_key,
    );
    let error = CertificateGeneration::from_files(
        "invalid-san",
        &["valid.example.test".into()],
        &invalid_san_files.chain,
        &invalid_san_files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::InvalidDnsSan { .. }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn strictly_rejects_incomplete_unordered_expired_non_ca_and_client_only_chains() {
    let temp = TempDir::new().unwrap();
    let root_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let root = build_certificate(
        "Chain Root",
        None,
        &root_key,
        &root_key,
        &[],
        false,
        true,
        false,
    );
    let intermediate_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let intermediate = build_certificate(
        "Chain Intermediate",
        Some(&root),
        &root_key,
        &intermediate_key,
        &[],
        false,
        true,
        false,
    );
    let leaf_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let leaf = build_certificate(
        "chain.example.test",
        Some(&intermediate),
        &intermediate_key,
        &leaf_key,
        &["chain.example.test"],
        false,
        false,
        true,
    );
    let valid = write_identity_material(
        temp.path(),
        "strict",
        &leaf,
        &[&intermediate, &root],
        &leaf_key,
    );
    CertificateGeneration::from_files(
        "strict",
        &["chain.example.test".into()],
        &valid.chain,
        &valid.key,
    )
    .unwrap();

    fs::write(&valid.chain, leaf.to_pem().unwrap()).unwrap();
    let error = CertificateGeneration::from_files(
        "incomplete",
        &["chain.example.test".into()],
        &valid.chain,
        &valid.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::IncompleteCertificateChain { .. }
    ));

    let unrelated_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let unrelated = build_certificate(
        "Unrelated Root",
        None,
        &unrelated_key,
        &unrelated_key,
        &[],
        false,
        true,
        false,
    );
    let unrelated_files =
        write_identity_material(temp.path(), "unrelated", &leaf, &[&unrelated], &leaf_key);
    let error = CertificateGeneration::from_files(
        "unrelated",
        &["chain.example.test".into()],
        &unrelated_files.chain,
        &unrelated_files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::InvalidChainIssuer { .. }));

    let reversed_chain = temp.path().join("reversed-chain.pem");
    let mut reversed = root.to_pem().unwrap();
    reversed.extend_from_slice(&intermediate.to_pem().unwrap());
    reversed.extend_from_slice(&leaf.to_pem().unwrap());
    fs::write(&reversed_chain, reversed).unwrap();
    let error = CertificateGeneration::from_files(
        "reversed",
        &["chain.example.test".into()],
        &reversed_chain,
        &valid.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::PrivateKeyMismatch { .. }));

    let expired_intermediate = build_certificate(
        "Expired Intermediate",
        Some(&root),
        &root_key,
        &intermediate_key,
        &[],
        true,
        true,
        false,
    );
    let expired_leaf = build_certificate(
        "expired-chain.example.test",
        Some(&expired_intermediate),
        &intermediate_key,
        &leaf_key,
        &["expired-chain.example.test"],
        false,
        false,
        true,
    );
    let expired_files = write_identity_material(
        temp.path(),
        "expired-intermediate",
        &expired_leaf,
        &[&expired_intermediate, &root],
        &leaf_key,
    );
    let error = CertificateGeneration::from_files(
        "expired-intermediate",
        &["expired-chain.example.test".into()],
        &expired_files.chain,
        &expired_files.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::ChainCertificateExpired { index: 1, .. }
    ));

    let non_ca = build_certificate(
        "Non-CA Intermediate",
        Some(&root),
        &root_key,
        &intermediate_key,
        &["issuer.example.test"],
        false,
        false,
        true,
    );
    let non_ca_leaf = build_certificate(
        "non-ca.example.test",
        Some(&non_ca),
        &intermediate_key,
        &leaf_key,
        &["non-ca.example.test"],
        false,
        false,
        true,
    );
    let non_ca_files = write_identity_material(
        temp.path(),
        "non-ca",
        &non_ca_leaf,
        &[&non_ca, &root],
        &leaf_key,
    );
    let error = CertificateGeneration::from_files(
        "non-ca",
        &["non-ca.example.test".into()],
        &non_ca_files.chain,
        &non_ca_files.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InvalidChainIssuer { .. } | TlsBuildError::NonCaChainIssuer { .. }
    ));

    let client_leaf = build_certificate(
        "client-only.example.test",
        Some(&intermediate),
        &intermediate_key,
        &leaf_key,
        &["client-only.example.test"],
        false,
        false,
        false,
    );
    let client_files = write_identity_material(
        temp.path(),
        "client-only",
        &client_leaf,
        &[&intermediate, &root],
        &leaf_key,
    );
    let error = CertificateGeneration::from_files(
        "client-only",
        &["client-only.example.test".into()],
        &client_files.chain,
        &client_files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::ChainVerification { .. }));
}

#[test]
fn installs_a_real_ecdsa_generation_with_an_intermediate() {
    let temp = TempDir::new().unwrap();
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    let root_key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let root = build_certificate(
        "ECDSA Root",
        None,
        &root_key,
        &root_key,
        &[],
        false,
        true,
        false,
    );
    let intermediate_key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let intermediate = build_certificate(
        "ECDSA Intermediate",
        Some(&root),
        &root_key,
        &intermediate_key,
        &[],
        false,
        true,
        false,
    );
    let leaf_key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let leaf = build_certificate(
        "ecdsa.example.test",
        Some(&intermediate),
        &intermediate_key,
        &leaf_key,
        &["ecdsa.example.test"],
        false,
        false,
        true,
    );
    let files = write_identity_material(
        temp.path(),
        "ecdsa",
        &leaf,
        &[&intermediate, &root],
        &leaf_key,
    );
    let generation = CertificateGeneration::from_files(
        "ecdsa",
        &["ecdsa.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap();
    assert_eq!(generation.metadata().intermediate_count, 2);

    let context = SslContextBuilder::new(SslMethod::tls()).unwrap().build();
    let mut ssl = Ssl::new(&context).unwrap();
    generation.install(&mut ssl).unwrap();
    assert_eq!(
        ssl.certificate().unwrap().to_der().unwrap(),
        leaf.to_der().unwrap()
    );
    assert!(ssl.private_key().unwrap().public_eq(&leaf_key));
}

#[test]
fn atomically_rejects_a_stale_generation_publication() {
    let temp = TempDir::new().unwrap();
    let first_files = write_identity(temp.path(), "first", "shared.example.test", false);
    let second_files = write_identity(temp.path(), "second", "shared.example.test", false);
    let third_files = write_identity(temp.path(), "third", "shared.example.test", false);
    let different_sans_files =
        write_identity(temp.path(), "different-sans", "other.example.test", false);
    let first = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["shared.example.test".into()],
            &first_files.chain,
            &first_files.key,
        )
        .unwrap(),
    );
    let second = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["shared.example.test".into()],
            &second_files.chain,
            &second_files.key,
        )
        .unwrap(),
    );
    let third = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["shared.example.test".into()],
            &third_files.chain,
            &third_files.key,
        )
        .unwrap(),
    );
    let active = ActiveCertificateGeneration::new(Arc::clone(&first));
    let stale = active.snapshot();

    let different_identity = Arc::new(
        CertificateGeneration::from_files(
            "different",
            &["shared.example.test".into()],
            &third_files.chain,
            &third_files.key,
        )
        .unwrap(),
    );
    let error = active
        .publish_if_current(&second, different_identity)
        .unwrap_err();
    assert_eq!(
        error,
        CertificatePublishError::IdentityMismatch {
            active_name: "shared".into(),
            replacement_name: "different".into(),
        }
    );
    assert!(Arc::ptr_eq(&active.snapshot(), &first));

    let different_sans = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["other.example.test".into()],
            &different_sans_files.chain,
            &different_sans_files.key,
        )
        .unwrap(),
    );
    let error = active
        .publish_if_current(&different_sans, Arc::clone(&different_sans))
        .unwrap_err();
    assert_eq!(
        error,
        CertificatePublishError::DnsNamesMismatch {
            identity: "shared".into(),
            active_dns_names: vec!["shared.example.test".into()],
            replacement_dns_names: vec!["other.example.test".into()],
        }
    );
    assert!(Arc::ptr_eq(&active.snapshot(), &first));

    active
        .publish_if_current(&stale, Arc::clone(&second))
        .unwrap();
    let error = active.publish_if_current(&stale, third).unwrap_err();
    let _: &CertificatePublishError = &error;
    assert!(matches!(
        error,
        CertificatePublishError::GenerationChanged {
            expected_revision,
            active_revision,
        } if expected_revision == first.metadata().revision
            && active_revision == second.metadata().revision
    ));
    assert!(Arc::ptr_eq(&active.snapshot(), &second));
}

#[test]
fn concurrent_snapshots_observe_only_complete_published_generations() {
    const READERS: usize = 8;
    const SNAPSHOTS_PER_READER: usize = 10_000;
    const PUBLICATIONS: usize = 2_000;

    let temp = TempDir::new().unwrap();
    let first_files = write_identity(temp.path(), "race-first", "race.example.test", false);
    let second_files = write_identity(temp.path(), "race-second", "race.example.test", false);
    let first = Arc::new(
        CertificateGeneration::from_files(
            "race",
            &["race.example.test".into()],
            &first_files.chain,
            &first_files.key,
        )
        .unwrap(),
    );
    let second = Arc::new(
        CertificateGeneration::from_files(
            "race",
            &["race.example.test".into()],
            &second_files.chain,
            &second_files.key,
        )
        .unwrap(),
    );
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::clone(&first)));
    let barrier = Arc::new(Barrier::new(READERS + 1));
    let mut readers = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let active = Arc::clone(&active);
        let barrier = Arc::clone(&barrier);
        let first = Arc::clone(&first);
        let second = Arc::clone(&second);
        readers.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..SNAPSHOTS_PER_READER {
                let generation = active.snapshot();
                assert!(Arc::ptr_eq(&generation, &first) || Arc::ptr_eq(&generation, &second));
            }
        }));
    }

    barrier.wait();
    for _ in 0..PUBLICATIONS {
        let current = active.snapshot();
        let replacement = if Arc::ptr_eq(&current, &first) {
            Arc::clone(&second)
        } else {
            Arc::clone(&first)
        };
        active.publish_if_current(&current, replacement).unwrap();
    }
    for reader in readers {
        reader.join().unwrap();
    }
}

#[test]
fn compiles_custom_ca_peer_policy_and_isolates_reuse() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "upstream", "origin.example.test", false);
    let custom_pool = upstream_pool(
        Some(files.ca.clone()),
        HttpVersion::Http11,
        HttpVersion::Http2,
    );
    let custom = prepare_upstream_tls(&custom_pool).unwrap().unwrap();
    let _: &UpstreamTlsPlan = &custom;
    let same = prepare_upstream_tls(&custom_pool).unwrap().unwrap();
    assert_eq!(custom.server_name(), "origin.example.test");
    assert_eq!(custom.min_http_version(), HttpVersion::Http11);
    assert_eq!(custom.max_http_version(), HttpVersion::Http2);
    assert!(custom.uses_custom_ca());
    assert_ne!(custom.group_key(), 0);
    assert_eq!(custom.group_key(), same.group_key());

    let mut peer = HttpPeer::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        false,
        String::new(),
    );
    peer.options.verify_cert = false;
    peer.options.verify_hostname = false;
    custom.apply_to_peer(&mut peer);
    assert!(peer.is_tls());
    assert_eq!(peer.sni, "origin.example.test");
    assert!(peer.options.verify_cert);
    assert!(peer.options.verify_hostname);
    assert!(peer.options.ca.is_some());
    assert!(peer.options.upstream_tls_configure_hook.is_some());
    assert_eq!(peer.options.alpn, ALPN::H2H1);
    assert_eq!(peer.group_key, custom.group_key());

    let mut same_peer = HttpPeer::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        false,
        String::new(),
    );
    same.apply_to_peer(&mut same_peer);
    assert!(Arc::ptr_eq(
        peer.options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("custom plan TLS configure hook"),
        same_peer
            .options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("same plan TLS configure hook"),
    ));

    let system_pool = upstream_pool(None, HttpVersion::Http11, HttpVersion::Http2);
    let system = prepare_upstream_tls(&system_pool).unwrap().unwrap();
    assert!(!system.uses_custom_ca());
    assert_ne!(custom.group_key(), system.group_key());
    let mut system_peer = HttpPeer::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        false,
        String::new(),
    );
    system.apply_to_peer(&mut system_peer);
    assert!(system_peer.options.ca.is_none());
    assert!(!Arc::ptr_eq(
        peer.options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("custom CA verification hook"),
        system_peer
            .options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("system root verification hook"),
    ));

    let h2_pool = upstream_pool(Some(files.ca), HttpVersion::Http2, HttpVersion::Http2);
    let h2 = prepare_upstream_tls(&h2_pool).unwrap().unwrap();
    assert_ne!(custom.group_key(), h2.group_key());
}

#[test]
fn rejects_malformed_custom_ca() {
    let temp = TempDir::new().unwrap();
    let malformed = temp.path().join("bad-ca.pem");
    fs::write(
        &malformed,
        b"-----BEGIN CERTIFICATE-----\nnot-a-certificate\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let pool = upstream_pool(Some(malformed), HttpVersion::Http11, HttpVersion::Http11);
    let error = prepare_upstream_tls(&pool).unwrap_err();
    assert!(matches!(error, TlsBuildError::CaParse { .. }));
    assert!(error.source().is_some());
}

#[test]
fn rejects_empty_duplicate_expired_and_non_ca_custom_anchors() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "anchors", "anchor.example.test", false);

    let empty = temp.path().join("empty-ca.pem");
    fs::write(&empty, []).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(empty),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::EmptyFile { .. }));

    let duplicate = temp.path().join("duplicate-ca.pem");
    let ca_pem = fs::read(&files.ca).unwrap();
    let mut duplicate_pem = ca_pem.clone();
    duplicate_pem.extend_from_slice(&ca_pem);
    fs::write(&duplicate, duplicate_pem).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(duplicate),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::DuplicateCaCertificate { index: 1, .. }
    ));

    let expired_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let expired_ca = build_certificate(
        "Expired Root",
        None,
        &expired_key,
        &expired_key,
        &[],
        true,
        true,
        false,
    );
    let expired_path = temp.path().join("expired-ca.pem");
    fs::write(&expired_path, expired_ca.to_pem().unwrap()).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(expired_path),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::CaCertificateExpired { index: 0, .. }
    ));

    let certificates = X509::stack_from_pem(&fs::read(&files.chain).unwrap()).unwrap();
    let non_ca_path = temp.path().join("non-ca-anchor.pem");
    fs::write(&non_ca_path, certificates[0].to_pem().unwrap()).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(non_ca_path),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::NonCaCertificate { index: 0, .. }
    ));
}

fn config_with_identity(files: &IdentityFiles) -> Config {
    Config {
        version: 1,
        management: None,
        certificates: vec![Certificate {
            name: "primary".into(),
            dns_names: vec!["www.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path: files.chain.clone(),
                private_key_path: files.key.clone(),
            },
        }],
        tls_profiles: vec![TlsProfile {
            name: "public".into(),
            certificates: vec!["primary".into()],
            default_certificate: "primary".into(),
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::H2, AlpnProtocol::Http11],
        }],
        listeners: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        cache_stores: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

#[cfg(unix)]
fn wait_for_condition(description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !condition() && std::time::Instant::now() < deadline {
        thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(condition(), "timed out waiting for {description}");
}

#[cfg(unix)]
struct CertbotTestLineage {
    name: String,
    live: PathBuf,
    archive: PathBuf,
}

#[cfg(unix)]
fn config_with_certbot_lineage(files: &CertbotTestLineage) -> Config {
    let mut config = config_with_identity(&IdentityFiles {
        chain: PathBuf::new(),
        key: PathBuf::new(),
        ca: PathBuf::new(),
    });
    config.certificates[0].dns_names = vec!["certbot.example.test".into()];
    config.certificates[0].source = CertificateSource::Certbot {
        live_directory_path: files.live.clone(),
        archive_directory_path: files.archive.clone(),
    };
    config
}

#[cfg(unix)]
fn write_certbot_lineage(root: &Path, name: &str, dns_name: &str) -> CertbotTestLineage {
    let material = root.join(format!("{name}-material"));
    fs::create_dir(&material).unwrap();
    let identity = write_identity(&material, name, dns_name, false);
    let live = root.join("live").join(name);
    let archive = root.join("archive").join(name);
    fs::create_dir_all(&live).unwrap();
    fs::create_dir_all(&archive).unwrap();

    write_certbot_revision(&archive, 1, &identity);
    let files = CertbotTestLineage {
        name: name.into(),
        live,
        archive,
    };
    set_live_revision(&files, 1);
    files
}

#[cfg(unix)]
fn write_certbot_revision(archive: &Path, revision: u64, identity: &IdentityFiles) {
    let fullchain = fs::read(&identity.chain).unwrap();
    let certificates = X509::stack_from_pem(&fullchain).unwrap();
    let cert = certificates[0].to_pem().unwrap();
    let chain = certificates[1..]
        .iter()
        .flat_map(|certificate| certificate.to_pem().unwrap())
        .collect::<Vec<_>>();

    fs::write(archive.join(format!("cert{revision}.pem")), cert).unwrap();
    fs::write(archive.join(format!("chain{revision}.pem")), chain).unwrap();
    fs::write(archive.join(format!("fullchain{revision}.pem")), fullchain).unwrap();
    write_private_key(
        &archive.join(format!("privkey{revision}.pem")),
        &fs::read(&identity.key).unwrap(),
    );
}

#[cfg(unix)]
fn write_new_certbot_revision(
    root: &Path,
    files: &CertbotTestLineage,
    revision: u64,
    stem: &str,
    dns_name: &str,
) {
    let material = root.join(format!("{stem}-material"));
    fs::create_dir(&material).unwrap();
    let identity = write_identity(&material, stem, dns_name, false);
    write_certbot_revision(&files.archive, revision, &identity);
}

#[cfg(unix)]
fn copy_certbot_revision(archive: &Path, from: u64, to: u64, reuse_key: bool) {
    use std::os::unix::fs::symlink;

    for stem in ["cert", "chain", "fullchain"] {
        fs::copy(
            archive.join(format!("{stem}{from}.pem")),
            archive.join(format!("{stem}{to}.pem")),
        )
        .unwrap();
    }
    let target = archive.join(format!("privkey{to}.pem"));
    if reuse_key {
        symlink(format!("privkey{from}.pem"), target).unwrap();
    } else {
        write_private_key(
            &target,
            &fs::read(archive.join(format!("privkey{from}.pem"))).unwrap(),
        );
    }
}

#[cfg(unix)]
fn set_live_revision(files: &CertbotTestLineage, revision: u64) {
    for stem in ["cert", "chain", "fullchain", "privkey"] {
        set_live_link(
            files,
            &format!("{stem}.pem"),
            &format!("{stem}{revision}.pem"),
        );
    }
}

#[cfg(unix)]
fn set_live_link(files: &CertbotTestLineage, name: &str, archive_name: &str) {
    set_raw_live_link(
        &files.live,
        name,
        &Path::new("../../archive")
            .join(&files.name)
            .join(archive_name),
    );
}

#[cfg(unix)]
fn set_raw_live_link(live: &Path, name: &str, target: &Path) {
    use std::os::unix::fs::symlink;

    let link = live.join(name);
    if fs::symlink_metadata(&link).is_ok() {
        fs::remove_file(&link).unwrap();
    }
    symlink(target, link).unwrap();
}

fn upstream_pool(
    ca_certificate_path: Option<PathBuf>,
    min: HttpVersion,
    max: HttpVersion,
) -> UpstreamPool {
    UpstreamPool {
        name: "origin".into(),
        endpoints: vec![UpstreamEndpoint::Socket {
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        }],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        tls: Some(UpstreamTls {
            server_name: "Origin.Example.Test".into(),
            ca_certificate_path,
        }),
        http_versions: HttpVersionPolicy { min, max },
    }
}

fn write_identity(directory: &Path, stem: &str, dns_name: &str, expired: bool) -> IdentityFiles {
    let ca_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let ca = build_certificate("Test Root", None, &ca_key, &ca_key, &[], false, true, false);
    let leaf_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let leaf = build_certificate(
        dns_name,
        Some(&ca),
        &ca_key,
        &leaf_key,
        &[dns_name],
        expired,
        false,
        true,
    );

    write_identity_material(directory, stem, &leaf, &[&ca], &leaf_key)
}

fn write_identity_material(
    directory: &Path,
    stem: &str,
    leaf: &X509,
    issuers: &[&X509],
    leaf_key: &PKey<Private>,
) -> IdentityFiles {
    let chain = directory.join(format!("{stem}-chain.pem"));
    let key = directory.join(format!("{stem}-key.pem"));
    let ca_path = directory.join(format!("{stem}-ca.pem"));
    let mut chain_pem = leaf.to_pem().unwrap();
    for issuer in issuers {
        chain_pem.extend_from_slice(&issuer.to_pem().unwrap());
    }
    fs::write(&chain, chain_pem).unwrap();
    write_private_key(&key, &leaf_key.private_key_to_pem_pkcs8().unwrap());
    fs::write(
        &ca_path,
        issuers
            .last()
            .expect("identity has an issuer")
            .to_pem()
            .unwrap(),
    )
    .unwrap();
    IdentityFiles {
        chain,
        key,
        ca: ca_path,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_certificate(
    common_name: &str,
    issuer: Option<&X509>,
    issuer_key: &PKey<Private>,
    subject_key: &PKey<Private>,
    dns_names: &[&str],
    expired: bool,
    is_ca: bool,
    server_auth: bool,
) -> X509 {
    build_certificate_with_leaf_usage(
        common_name,
        issuer,
        issuer_key,
        subject_key,
        dns_names,
        expired,
        is_ca,
        server_auth,
        LeafKeyUsage::DigitalSignature,
    )
}

#[derive(Clone, Copy)]
enum LeafKeyUsage {
    DigitalSignature,
    KeyEncipherment,
}

#[allow(clippy::too_many_arguments)]
fn build_certificate_with_leaf_usage(
    common_name: &str,
    issuer: Option<&X509>,
    issuer_key: &PKey<Private>,
    subject_key: &PKey<Private>,
    dns_names: &[&str],
    expired: bool,
    is_ca: bool,
    server_auth: bool,
    leaf_key_usage: LeafKeyUsage,
) -> X509 {
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", common_name).unwrap();
    let name = name.build();
    let mut builder = X509::builder().unwrap();
    builder.set_version(2).unwrap();
    let serial = Asn1Integer::from_bn(&BigNum::from_u32(1).unwrap()).unwrap();
    builder.set_serial_number(&serial).unwrap();
    builder.set_subject_name(&name).unwrap();
    builder
        .set_issuer_name(issuer.map_or(name.as_ref(), |certificate| certificate.subject_name()))
        .unwrap();
    builder.set_pubkey(subject_key).unwrap();
    let not_before = Asn1Time::from_str_x509("20200101000000Z").unwrap();
    let not_after = Asn1Time::from_str_x509(if expired {
        "20210101000000Z"
    } else {
        "20490101000000Z"
    })
    .unwrap();
    builder.set_not_before(&not_before).unwrap();
    builder.set_not_after(&not_after).unwrap();
    if is_ca {
        builder
            .append_extension(BasicConstraints::new().critical().ca().build().unwrap())
            .unwrap();
        builder
            .append_extension(
                KeyUsage::new()
                    .critical()
                    .key_cert_sign()
                    .crl_sign()
                    .build()
                    .unwrap(),
            )
            .unwrap();
    } else {
        builder
            .append_extension(BasicConstraints::new().critical().build().unwrap())
            .unwrap();
        let mut key_usage = KeyUsage::new();
        key_usage.critical();
        match leaf_key_usage {
            LeafKeyUsage::DigitalSignature => key_usage.digital_signature(),
            LeafKeyUsage::KeyEncipherment => key_usage.key_encipherment(),
        };
        builder
            .append_extension(key_usage.build().unwrap())
            .unwrap();
        let extended_key_usage = if server_auth {
            ExtendedKeyUsage::new().server_auth().build().unwrap()
        } else {
            ExtendedKeyUsage::new().client_auth().build().unwrap()
        };
        builder.append_extension(extended_key_usage).unwrap();
    }
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&builder.x509v3_context(issuer.map(AsRef::as_ref), None))
        .unwrap();
    builder.append_extension(subject_key_identifier).unwrap();
    let authority_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(true)
        .issuer(true)
        .build(&builder.x509v3_context(issuer.map(AsRef::as_ref), None))
        .unwrap();
    builder.append_extension(authority_key_identifier).unwrap();
    if !dns_names.is_empty() {
        let mut subject_alternative_name = SubjectAlternativeName::new();
        for dns_name in dns_names {
            subject_alternative_name.dns(dns_name);
        }
        let extension = subject_alternative_name
            .build(&builder.x509v3_context(issuer.map(AsRef::as_ref), None))
            .unwrap();
        builder.append_extension(extension).unwrap();
    }
    builder.sign(issuer_key, MessageDigest::sha256()).unwrap();
    builder.build()
}

fn write_private_key(path: &Path, pem: &[u8]) {
    fs::write(path, pem).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
