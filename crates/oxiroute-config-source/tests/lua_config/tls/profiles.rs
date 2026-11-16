use oxiroute_config::{AlpnProtocol, ConfigError, Protocol};

use crate::common::{changed, error_from};
use crate::lua_support::load_lua;

#[test]
fn accepts_only_the_supported_alpn_policies() {
    for (policy, expected) in [
        (r#"{ "http/1.1" }"#, vec![AlpnProtocol::Http11]),
        (r#"{ "h2" }"#, vec![AlpnProtocol::H2]),
        (
            r#"{ "h2", "http/1.1" }"#,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
        ),
    ] {
        let source = changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            &format!("      alpn = {policy},"),
        );
        let config = load_lua(&source).expect("supported ALPN policy");
        assert_eq!(config.tls_profiles[0].alpn, expected);
    }

    for policy in [
        r"{}",
        r#"{ "h2", "h2" }"#,
        r#"{ "http/1.1", "http/1.1" }"#,
        r#"{ "http/1.1", "h2" }"#,
        r#"{ "h2", "http/1.1", "h2" }"#,
    ] {
        let source = changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            &format!("      alpn = {policy},"),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidTlsProfileAlpn { profile } if profile == "web-tls"
        ));
    }
}

#[test]
fn validates_tls_policy_bounds_and_paths() {
    for (field, policy) in [
        (
            "cipher_list",
            r#"{ cipher_list = "", session_tickets = false, prefer_server_ciphers = true }"#,
        ),
        (
            "session_cache.name",
            r#"{ session_cache = { name = "invalid:name", size_bytes = 10485760 }, session_tickets = false, prefer_server_ciphers = true }"#,
        ),
        (
            "session_cache.size_bytes",
            r#"{ session_cache = { name = "SSL", size_bytes = 255 }, session_tickets = false, prefer_server_ciphers = true }"#,
        ),
        (
            "session_timeout_seconds",
            r"{ session_timeout_seconds = 0, session_tickets = false, prefer_server_ciphers = true }",
        ),
    ] {
        let source = changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            &format!("      alpn = {{ \"h2\", \"http/1.1\" }},\n      policy = {policy},"),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidTlsProfilePolicy {
                profile,
                field: invalid_field,
                ..
            } if profile == "web-tls" && invalid_field == field
        ));
    }

    let source = changed(
        "      alpn = { \"h2\", \"http/1.1\" },",
        "      alpn = { \"h2\", \"http/1.1\" },\n      policy = { dh_parameters_path = \"relative.pem\", session_tickets = false, prefer_server_ciphers = true },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::InvalidFilePath {
            kind: "TLS profile",
            name,
            field: "policy.dh_parameters_path",
            ..
        } if name == "web-tls"
    ));
}

#[test]
fn validates_tls_profile_and_listener_references() {
    let source = changed(
        "      certificates = { \"web-certificate\" },",
        "      certificates = { \"missing\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::UnknownTlsProfileCertificate {
            profile,
            certificate
        } if profile == "web-tls" && certificate == "missing"
    ));

    let source = changed(
        "      certificates = { \"web-certificate\" },",
        "      certificates = {},",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::EmptyTlsProfileCertificates { profile } if profile == "web-tls"
    ));

    let source = changed(
        "      certificates = { \"web-certificate\" },",
        "      certificates = { \"web-certificate\", \"web-certificate\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateTlsProfileCertificate {
            profile,
            certificate
        } if profile == "web-tls" && certificate == "web-certificate"
    ));

    let source = changed(
        "      default_certificate = \"web-certificate\",",
        "      default_certificate = \"missing\",",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TlsProfileDefaultNotListed {
            profile,
            certificate
        } if profile == "web-tls" && certificate == "missing"
    ));

    let source = changed(
        "      tls_profile = \"web-tls\",",
        "      tls_profile = \"missing\",",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::UnknownListenerTlsProfile { listener, profile }
            if listener == "web" && profile == "missing"
    ));

    for (fragment, listener, protocol) in [
        (
            "      service = \"database\",\n      max_connections = 1000,",
            "database",
            Protocol::Tcp,
        ),
        (
            "      protocol = \"rtmp\",\n      service = \"live\",\n      max_connections = 500,",
            "live",
            Protocol::Rtmp,
        ),
    ] {
        let replacement = fragment.replace(
            "      max_connections",
            "      tls_profile = \"web-tls\",\n      max_connections",
        );
        let source = changed(fragment, &replacement);
        assert!(matches!(
            error_from(&source),
            ConfigError::UnexpectedListenerTlsProfile {
                listener: actual_listener,
                protocol: actual_protocol,
                profile,
            } if actual_listener == listener
                && actual_protocol == protocol
                && profile == "web-tls"
        ));
    }
}

#[test]
fn rejects_dns_name_ownership_overlap_within_a_tls_profile() {
    for dns_name in ["www.example.test", "*.example.test"] {
        let source = changed(
            "    },\n  },\n  tls_profiles = {",
            &format!(
                r#"    }},
    {{
      name = "overlapping-certificate",
      dns_names = {{ "{dns_name}" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/overlapping-chain.pem",
        private_key_path = "/etc/oxiroute/overlapping-key.pem",
      }},
    }},
  }},
  tls_profiles = {{"#
            ),
        );
        let source = source.replace(
            "      certificates = { \"web-certificate\" },",
            "      certificates = { \"web-certificate\", \"overlapping-certificate\" },",
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::OverlappingTlsProfileDnsName {
                profile,
                dns_name: actual_dns_name,
                first_certificate,
                second_certificate,
            } if profile == "web-tls"
                && actual_dns_name == dns_name
                && first_certificate == "web-certificate"
                && second_certificate == "overlapping-certificate"
        ));
    }
}

#[test]
fn permits_shared_canonical_ip_identities_within_a_tls_profile() {
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"::ffff:c000:020a\" },",
    );
    let source = source.replace(
        "    },\n  },\n  tls_profiles = {",
        r#"    },
    {
      name = "ip-certificate",
      dns_names = { "192.0.2.10" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/ip-chain.pem",
        private_key_path = "/etc/oxiroute/ip-key.pem",
      },
    },
  },
  tls_profiles = {"#,
    );
    let source = source.replace(
        "      certificates = { \"web-certificate\" },",
        "      certificates = { \"web-certificate\", \"ip-certificate\" },",
    );

    let config = load_lua(&source).expect("shared IP identities do not participate in SNI");
    assert_eq!(config.certificates[0].dns_names, ["192.0.2.10"]);
    assert_eq!(config.certificates[1].dns_names, ["192.0.2.10"]);
}
