use oxiroute_config::{
    AcmeChallengeType, AcmeKeyType, CertificateSource, ConfigError, SelfSignedKeyType,
};

use crate::common::{changed, error_from, lua_error_from};
use crate::lua_support::{load_lua, render_lua};

fn with_certbot_source(live_directory_path: &str, archive_directory_path: &str) -> String {
    changed(
        r#"      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },"#,
        &format!(
            r#"      source = {{
        type = "certbot",
        live_directory_path = "{live_directory_path}",
        archive_directory_path = "{archive_directory_path}",
      }},"#
        ),
    )
}

fn with_self_signed_source(fields: &str) -> String {
    changed(
        r#"      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },"#,
        &format!(
            r#"      source = {{
        type = "self_signed_development",
        {fields}
      }},"#
        ),
    )
}

fn with_acme_source(fields: &str) -> String {
    changed(
        r#"      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },"#,
        &format!(
            r#"      source = {{
        type = "acme_managed",
        directory_url = "https://acme.example.test/directory",
        state_root = "/var/lib/oxiroute/acme",
        terms_agreed = true,
        allowed_dns_suffixes = {{ "example.test" }},
        {fields}
      }},"#
        ),
    )
    .replace(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"WWW.EXAMPLE.TEST\" },",
    )
}

#[test]
fn self_signed_development_source_defaults_and_renders_explicitly() {
    let config = load_lua(&with_self_signed_source("")).expect("default development source");
    let CertificateSource::SelfSignedDevelopment {
        validity_days,
        key_type,
    } = config.certificates[0].source
    else {
        panic!("development source");
    };
    assert_eq!(validity_days, 7);
    assert_eq!(key_type, SelfSignedKeyType::EcdsaP256);

    let rendered = render_lua(&config).expect("render development source");
    assert!(rendered.contains("type = \"self_signed_development\""));
    assert!(rendered.contains("validity_days = 7"));
    assert!(rendered.contains("key_type = \"ecdsa_p256\""));
    assert_eq!(
        load_lua(&rendered).expect("reload development source"),
        config
    );
}

#[test]
fn self_signed_development_source_accepts_key_type_and_rejects_unbounded_validity() {
    let config = load_lua(&with_self_signed_source(
        "validity_days = 14,\n        key_type = \"rsa_2048\",",
    ))
    .expect("bounded RSA development source");
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::SelfSignedDevelopment {
            validity_days: 14,
            key_type: SelfSignedKeyType::Rsa2048,
        }
    ));

    for validity_days in [0, 31] {
        let error = load_lua(&with_self_signed_source(&format!(
            "validity_days = {validity_days},"
        )))
        .expect_err("out-of-bounds development validity");
        assert!(matches!(
            error,
            ConfigError::InvalidSelfSignedValidityDays { value, .. } if value == validity_days
        ));
    }
}

#[test]
fn managed_acme_source_round_trips_and_rejects_unsafe_policy_values() {
    let config = load_lua(&with_acme_source(
        "contacts = { \"mailto:ops@example.test\" },\n        challenge = \"http01\",\n        key_type = \"rsa_2048\",",
    ))
    .expect("managed ACME source");
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::AcmeManaged {
            challenge: AcmeChallengeType::Http01,
            key_type: AcmeKeyType::Rsa2048,
            dns01: None,
            ..
        }
    ));
    let rendered = render_lua(&config).expect("render managed ACME source");
    assert!(rendered.contains("type = \"acme_managed\""));
    assert_eq!(
        load_lua(&rendered).expect("reload managed ACME source"),
        config
    );

    for (field, value) in [
        ("directory_url", "directory_url = \"https://\""),
        (
            "directory_url",
            "directory_url = \"http://acme.example.test/directory\"",
        ),
        (
            "allowed_dns_suffixes",
            "allowed_dns_suffixes = { \"other.test\" }",
        ),
        ("retained_revisions", "retained_revisions = 0"),
    ] {
        let error = load_lua(&with_acme_source(value)).expect_err("invalid managed ACME source");
        assert!(
            matches!(
                (field, &error),
                ("directory_url", ConfigError::InvalidAcmeDirectoryUrl { .. })
                    | (
                        "allowed_dns_suffixes",
                        ConfigError::AcmeIdentifierOutsidePolicy { .. }
                    )
                    | (
                        "retained_revisions",
                        ConfigError::InvalidAcmeRetention { .. }
                    )
            ),
            "unexpected error for {field}: {error:?}"
        );
    }

    let tls_alpn_config = load_lua(&with_acme_source(
        "contacts = { \"mailto:ops@example.test\" },\n        challenge = \"tls_alpn01\",\n        key_type = \"ecdsa_p256\",",
    ))
    .expect("managed TLS-ALPN-01 source");
    assert!(matches!(
        tls_alpn_config.certificates[0].source,
        CertificateSource::AcmeManaged {
            challenge: AcmeChallengeType::TlsAlpn01,
            dns01: None,
            ..
        }
    ));
    assert_eq!(
        load_lua(&render_lua(&tls_alpn_config).expect("render TLS-ALPN-01 source"))
            .expect("reload TLS-ALPN-01 source"),
        tls_alpn_config
    );

    let dns_config = with_acme_source(
        "contacts = { \"mailto:ops@example.test\" },\n        challenge = \"dns01\",\n        key_type = \"ecdsa_p256\",\n        dns01 = { provider = \"fake\", credential_file = \"/etc/oxiroute/dns-credentials\", timeout_seconds = 30 },",
    )
    .replace(
        "      dns_names = { \"WWW.EXAMPLE.TEST\" },",
        "      dns_names = { \"*.EXAMPLE.TEST\" },",
    );
    let dns_config = load_lua(&dns_config).expect("managed DNS-01 source");
    assert!(matches!(
        &dns_config.certificates[0].source,
        CertificateSource::AcmeManaged {
            challenge: AcmeChallengeType::Dns01,
            dns01: Some(dns01),
            ..
        } if dns01.provider == "fake"
            && dns01.credential_file.as_path()
                == std::path::Path::new("/etc/oxiroute/dns-credentials")
            && dns01.timeout_seconds == 30
    ));
    assert_eq!(
        load_lua(&render_lua(&dns_config).expect("render DNS-01 source"))
            .expect("reload DNS-01 source"),
        dns_config
    );

    let ip_acme_config = with_acme_source("").replace(
        "      dns_names = { \"WWW.EXAMPLE.TEST\" },",
        "      dns_names = { \"192.0.2.10\" },",
    );
    assert!(matches!(
        load_lua(&ip_acme_config),
        Err(ConfigError::AcmeIdentifierUnsupported { .. })
    ));
}

#[test]
fn loads_the_canonical_certbot_certificate_source() {
    let source = with_certbot_source(
        "/etc/letsencrypt/live/www.example.test",
        "/etc/letsencrypt/archive/www.example.test",
    );
    let config = load_lua(&source).expect("canonical Certbot certificate source");

    assert_eq!(
        config.certificates[0].source,
        CertificateSource::Certbot {
            live_directory_path: "/etc/letsencrypt/live/www.example.test".into(),
            archive_directory_path: "/etc/letsencrypt/archive/www.example.test".into(),
        }
    );
}

#[test]
fn validates_certbot_directory_paths_lexically() {
    let boundary = format!("/{}", "a".repeat(4_095));
    load_lua(&with_certbot_source(
        &boundary,
        "/etc/letsencrypt/archive/www.example.test",
    ))
    .expect("4096-byte Certbot live directory path");
    load_lua(&with_certbot_source(
        "/etc/letsencrypt/live/www.example.test",
        &boundary,
    ))
    .expect("4096-byte Certbot archive directory path");

    for (field, path) in [
        (
            "source.live_directory_path",
            "etc/letsencrypt/live/name".into(),
        ),
        ("source.live_directory_path", "/".into()),
        ("source.live_directory_path", "/etc//live/name".into()),
        ("source.live_directory_path", "/etc/live/name/".into()),
        ("source.live_directory_path", "/etc/./live/name".into()),
        ("source.live_directory_path", "/etc/../live/name".into()),
        ("source.live_directory_path", r"/etc/live/\0name".into()),
        (
            "source.live_directory_path",
            format!("/{}", "a".repeat(4_096)),
        ),
        (
            "source.archive_directory_path",
            "etc/letsencrypt/archive/name".into(),
        ),
        ("source.archive_directory_path", "/".into()),
        ("source.archive_directory_path", "/etc//archive/name".into()),
        ("source.archive_directory_path", "/etc/archive/name/".into()),
        (
            "source.archive_directory_path",
            "/etc/./archive/name".into(),
        ),
        (
            "source.archive_directory_path",
            "/etc/../archive/name".into(),
        ),
        (
            "source.archive_directory_path",
            r"/etc/archive/\0name".into(),
        ),
        (
            "source.archive_directory_path",
            format!("/{}", "a".repeat(4_096)),
        ),
    ] {
        let (live, archive) = if field == "source.live_directory_path" {
            (path.as_str(), "/etc/letsencrypt/archive/name")
        } else {
            ("/etc/letsencrypt/live/name", path.as_str())
        };
        assert!(matches!(
            error_from(&with_certbot_source(live, archive)),
            ConfigError::InvalidFilePath {
                kind: "certificate",
                field: actual_field,
                ..
            } if actual_field == field
        ));
    }

    assert!(matches!(
        error_from(&with_certbot_source(
            "/etc/letsencrypt/lineage/name",
            "/etc/letsencrypt/lineage/name",
        )),
        ConfigError::DuplicateCertbotDirectories { certificate }
            if certificate == "web-certificate"
    ));
}

#[test]
fn rejects_incomplete_or_noncanonical_certbot_source_objects() {
    for source in [
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        live_directory_path = \"/etc/letsencrypt/live/name\",\n",
            "",
        ),
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        archive_directory_path = \"/etc/letsencrypt/archive/name\",\n",
            "",
        ),
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        archive_directory_path = \"/etc/letsencrypt/archive/name\",",
            "        archive_directory_path = \"/etc/letsencrypt/archive/name\",\n        unexpected = true,",
        ),
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        live_directory_path = \"/etc/letsencrypt/live/name\",",
            "        live_directory_path = \"/etc/letsencrypt/live/name\",\n        certificate_chain_path = \"/etc/oxiroute/chain.pem\",",
        ),
    ] {
        assert!(matches!(
            lua_error_from(&source),
            oxiroute_config_source::LuaConfigError::Lua(_)
        ));
    }
}
