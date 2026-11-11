#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn prepares_metadata_redacted_generation_and_callback_settings() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let config = config_with_identity(&files);

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
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
    let mut settings = profile.tls_settings(0).unwrap();
    assert!(settings.options().contains(SslOptions::NO_TICKET));
    assert!(
        settings
            .options()
            .contains(SslOptions::CIPHER_SERVER_PREFERENCE)
    );
    assert_eq!(
        settings.set_session_cache_mode(SslSessionCacheMode::SERVER),
        SslSessionCacheMode::OFF
    );
    assert_eq!(settings.set_session_timeout(1), 7_200);
}

#[test]
fn applies_explicit_server_tls_policy_to_openssl_settings() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let dh_parameters_path = temp.path().join("dhparam.pem");
    fs::write(
        &dh_parameters_path,
        Dh::get_2048_256().unwrap().params_to_pem().unwrap(),
    )
    .unwrap();
    let mut config = config_with_identity(&files);
    config.tls_profiles[0].policy = TlsPolicy {
        cipher_list: Some("ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:DHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES256-GCM-SHA384".into()),
        dh_parameters_path: Some(dh_parameters_path),
        client_auth: TlsClientAuthPolicy::default(),
        session_cache: Some(TlsSessionCache {
            name: "le_nginx_SSL".into(),
            size_bytes: 10 * 1024 * 1024,
        }),
        session_timeout_seconds: Some(86_400),
        session_tickets: false,
        prefer_server_ciphers: false,
        backend: oxiroute_config::TlsBackend::Openssl,
        key_exchange_groups: None,
        ktls: oxiroute_config::KtlsMode::Off,
    };

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let profile = prepared.profiles().get("public").unwrap();
    let mut settings = profile.tls_settings(0).unwrap();

    assert_eq!(profile.policy(), &config.tls_profiles[0].policy);
    assert_eq!(settings.set_session_cache_size(1), 40_960);
    assert_eq!(
        settings.set_session_cache_mode(SslSessionCacheMode::OFF),
        SslSessionCacheMode::SERVER
    );
    assert_eq!(settings.set_session_timeout(1), 86_400);
    assert!(settings.options().contains(SslOptions::NO_TICKET));
    assert!(
        !settings
            .options()
            .contains(SslOptions::CIPHER_SERVER_PREFERENCE)
    );
}

#[test]
fn downstream_client_auth_defaults_to_disabled_and_is_reported_as_configured_policy() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let config = config_with_identity(&files);

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let profile = prepared.profiles().get("public").unwrap();
    assert_eq!(profile.client_auth_mode(), TlsClientAuthMode::Disabled);
    assert!(!profile.client_auth_ca_configured());
    assert_eq!(profile.client_auth_allowed_dns_name_count(), 0);
    assert!(profile.tls_settings(0).is_ok());
}

#[test]
fn client_auth_ca_is_loaded_once_into_an_immutable_profile_snapshot() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let mut config = config_with_identity(&files);
    config.tls_profiles[0].policy.client_auth = TlsClientAuthPolicy {
        mode: TlsClientAuthMode::Required,
        ca_certificate_path: Some(files.ca.clone()),
        allowed_dns_names: vec!["client.example.test".into()],
    };

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let profile = prepared.profiles().get("public").unwrap();
    assert_eq!(profile.client_auth_mode(), TlsClientAuthMode::Required);
    assert!(profile.client_auth_ca_configured());
    assert_eq!(profile.client_auth_allowed_dns_name_count(), 1);
    assert!(profile.tls_settings(0).is_ok());

    fs::write(&files.ca, b"not a CA bundle").unwrap();
    assert!(profile.tls_settings(0).is_ok());
    assert!(matches!(
        prepare_tls(&config.clone().validate().expect("valid TLS test config")),
        Err(TlsBuildError::ClientCaParse { .. } | TlsBuildError::InvalidPem { .. })
    ));
}

#[test]
fn client_auth_rejects_oversized_and_non_ca_bundles_before_listener_start() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let mut config = config_with_identity(&files);
    config.tls_profiles[0].policy.client_auth = TlsClientAuthPolicy {
        mode: TlsClientAuthMode::Required,
        ca_certificate_path: Some(files.ca.clone()),
        allowed_dns_names: Vec::new(),
    };

    fs::write(&files.ca, vec![b'x'; MAX_CA_CERTIFICATE_BYTES + 1]).unwrap();
    assert!(matches!(
        prepare_tls(&config.clone().validate().expect("valid TLS test config")),
        Err(TlsBuildError::FileTooLarge {
            kind: "client CA bundle",
            limit: MAX_CA_CERTIFICATE_BYTES,
            ..
        })
    ));

    let files = write_identity(temp.path(), "non-ca", "www.example.test", false);
    config.certificates[0].source = CertificateSource::Files {
        certificate_chain_path: files.chain.clone(),
        private_key_path: files.key.clone(),
    };
    config.tls_profiles[0]
        .policy
        .client_auth
        .ca_certificate_path = Some(files.chain);
    assert!(matches!(
        prepare_tls(&config.clone().validate().expect("valid TLS test config")),
        Err(TlsBuildError::NonCaClientCertificate { index: 0, .. })
    ));
}

#[test]
fn rejects_oversized_dh_parameters_during_runtime_planning() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let dh_parameters_path = temp.path().join("oversized-dhparam.pem");
    fs::write(&dh_parameters_path, vec![b'x'; MAX_DH_PARAMETERS_BYTES + 1]).unwrap();
    let mut config = config_with_identity(&files);
    config.tls_profiles[0].policy.dh_parameters_path = Some(dh_parameters_path);

    assert!(matches!(
        prepare_tls(&config.clone().validate().expect("valid TLS test config")),
        Err(TlsBuildError::FileTooLarge {
            kind: "DH parameters",
            limit: MAX_DH_PARAMETERS_BYTES,
            ..
        })
    ));
}

#[test]
fn generates_explicit_development_certificate_with_exact_sans_and_redacted_debug() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "unused", "unused.example.test", false);
    let mut config = config_with_identity(&files);
    config.certificates[0].dns_names = vec!["dev.example.test".into(), "*.dev.example.test".into()];
    config.certificates[0].source = CertificateSource::SelfSignedDevelopment {
        validity_days: 7,
        key_type: SelfSignedKeyType::EcdsaP256,
    };

    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let active = prepared.certificates().get("primary").unwrap();
    let generation = active.snapshot();
    assert_eq!(
        generation.metadata().dns_names,
        ["*.dev.example.test", "dev.example.test"]
    );
    assert_eq!(generation.metadata().intermediate_count, 0);
    assert_eq!(
        prepared
            .profiles()
            .get("public")
            .unwrap()
            .selected_generation(Some("API.dev.example.test"))
            .metadata()
            .name,
        "primary"
    );
    assert!(prepared.file_reconcilers().is_empty());
    assert!(prepared.certbot_reconcilers().is_empty());

    let debug = format!("{generation:?}");
    assert!(!debug.contains("PRIVATE KEY"));
    assert!(!debug.contains("private_key"));
}
