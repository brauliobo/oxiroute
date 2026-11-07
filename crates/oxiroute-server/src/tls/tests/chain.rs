#[allow(clippy::wildcard_imports)]
use super::*;

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
fn requires_declared_dns_names_to_be_a_subset_of_valid_dns_sans() {
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

    let generation = CertificateGeneration::from_files(
        "missing-declaration",
        &["www.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap();
    assert_eq!(generation.metadata().dns_names, ["www.example.test"]);

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
fn accepts_ip_sans_canonicalizes_ipv6_mapped_ipv4_and_uses_ips_only_as_the_default() {
    let temp = TempDir::new().unwrap();
    let ca_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let ca = build_certificate(
        "IP SAN Root",
        None,
        &ca_key,
        &ca_key,
        &[],
        false,
        true,
        false,
    );
    let leaf_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let leaf = build_certificate_with_sans(
        "ip-identity",
        Some(&ca),
        &ca_key,
        &leaf_key,
        &[],
        &["::ffff:192.0.2.10", "2001:0DB8:0:0:0:0:0:1"],
        false,
        false,
        true,
    );
    let files = write_identity_material(temp.path(), "ip-sans", &leaf, &[&ca], &leaf_key);
    let mut config = config_with_identity(&files);
    config.certificates[0].dns_names = vec!["2001:0db8:0:0:0:0:0:1".into(), "192.0.2.10".into()];

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let generation = prepared.certificates()["primary"].snapshot();
    assert_eq!(
        generation.metadata().dns_names,
        ["192.0.2.10", "2001:db8::1"]
    );
    let profile = &prepared.profiles()["public"];
    assert_eq!(profile.selected_generation(None).metadata().name, "primary");
    assert_eq!(
        profile
            .selected_generation(Some("192.0.2.10"))
            .metadata()
            .name,
        "primary"
    );
}

#[test]
fn rejects_dns_ip_san_type_mismatches_and_missing_declared_identities() {
    let temp = TempDir::new().unwrap();
    let ca_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let ca = build_certificate(
        "Typed SAN Root",
        None,
        &ca_key,
        &ca_key,
        &[],
        false,
        true,
        false,
    );
    let leaf_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let dns_ip_text = build_certificate(
        "dns-ip-text",
        Some(&ca),
        &ca_key,
        &leaf_key,
        &["192.0.2.10"],
        false,
        false,
        true,
    );
    let dns_ip_text_files =
        write_identity_material(temp.path(), "dns-ip-text", &dns_ip_text, &[&ca], &leaf_key);
    let error = CertificateGeneration::from_files(
        "type-mismatch",
        &["192.0.2.10".into()],
        &dns_ip_text_files.chain,
        &dns_ip_text_files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::InvalidDnsSan { .. }));

    let mixed_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let mixed = build_certificate_with_sans(
        "mixed-identities",
        Some(&ca),
        &ca_key,
        &mixed_key,
        &["www.example.test"],
        &["192.0.2.10"],
        false,
        false,
        true,
    );
    let mixed_files = write_identity_material(temp.path(), "mixed", &mixed, &[&ca], &mixed_key);
    let error = CertificateGeneration::from_files(
        "wildcard-ip",
        &["*.192.0.2.10".into()],
        &mixed_files.chain,
        &mixed_files.key,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::InvalidDeclaredDnsNames { .. }
    ));

    let subset = CertificateGeneration::from_files(
        "identity-subset",
        &["www.example.test".into()],
        &mixed_files.chain,
        &mixed_files.key,
    )
    .unwrap();
    assert_eq!(subset.metadata().dns_names, ["www.example.test"]);

    let error = CertificateGeneration::from_files(
        "identity-set-mismatch",
        &[
            "www.example.test".into(),
            "192.0.2.10".into(),
            "api.example.test".into(),
        ],
        &mixed_files.chain,
        &mixed_files.key,
    )
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::DnsSanMismatch { .. }));

    let generation = CertificateGeneration::from_files(
        "mixed-identities",
        &["192.0.2.10".into(), "WWW.EXAMPLE.TEST".into()],
        &mixed_files.chain,
        &mixed_files.key,
    )
    .unwrap();
    assert_eq!(
        generation.metadata().dns_names,
        ["www.example.test", "192.0.2.10"]
    );
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
