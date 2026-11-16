use oxiroute_config::ConfigError;

use crate::common::{changed, error_from, lua_error_from};
use crate::lua_support::load_lua;

#[test]
fn accepts_certificate_and_tls_profile_cardinality_boundaries() {
    let certificates = (0..256)
        .map(|index| {
            format!(
                r#"{{
      name = "certificate-{index}",
      dns_names = {{ "certificate-{index}.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/chain-{index}.pem",
        private_key_path = "/etc/oxiroute/key-{index}.pem",
      }},
    }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source =
        format!("return {{ version = 1, listeners = {{}}, certificates = {{ {certificates} }} }}");
    let config = load_lua(&source).expect("256 certificates");
    assert_eq!(config.certificates.len(), 256);

    let profiles = (0..256)
        .map(|index| {
            format!(
                r#"{{ name = "profile-{index}", certificates = {{ "shared" }}, default_certificate = "shared" }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  certificates = {{
    {{
      name = "shared",
      dns_names = {{ "shared.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/shared-chain.pem",
        private_key_path = "/etc/oxiroute/shared-key.pem",
      }},
    }},
  }},
  tls_profiles = {{ {profiles} }},
}}"#
    );
    let config = load_lua(&source).expect("256 TLS profiles");
    assert_eq!(config.tls_profiles.len(), 256);
}

#[test]
fn rejects_excessive_certificate_and_tls_profile_cardinality() {
    let certificates = (0..257)
        .map(|index| {
            format!(
                r#"{{
      name = "certificate-{index}",
      dns_names = {{ "certificate-{index}.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/chain-{index}.pem",
        private_key_path = "/etc/oxiroute/key-{index}.pem",
      }},
    }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source =
        format!("return {{ version = 1, listeners = {{}}, certificates = {{ {certificates} }} }}");
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyCertificates
    ));

    let profiles = (0..257)
        .map(|index| {
            format!(
                r#"{{ name = "profile-{index}", certificates = {{ "shared" }}, default_certificate = "shared" }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  certificates = {{
    {{
      name = "shared",
      dns_names = {{ "shared.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/shared-chain.pem",
        private_key_path = "/etc/oxiroute/shared-key.pem",
      }},
    }},
  }},
  tls_profiles = {{ {profiles} }},
}}"#
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyTlsProfiles
    ));
}

#[test]
fn validates_and_normalizes_certificate_dns_names() {
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },\n",
        "",
    );
    let error = lua_error_from(&source);
    assert!(matches!(
        error,
        oxiroute_config_source::LuaConfigError::Lua(_)
    ));
    assert!(error.to_string().contains("missing field `dns_names`"));

    let names = (0..100)
        .map(|index| format!(r#""HOST-{index}.EXAMPLE.TEST""#))
        .collect::<Vec<_>>()
        .join(", ");
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        &format!("      dns_names = {{ {names} }},"),
    );
    let config = load_lua(&source).expect("100 unique certificate DNS names");
    assert_eq!(config.certificates[0].dns_names.len(), 100);
    assert_eq!(config.certificates[0].dns_names[0], "host-0.example.test");

    let too_many = (0..101)
        .map(|index| format!(r#""host-{index}.example.test""#))
        .collect::<Vec<_>>()
        .join(", ");
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        &format!("      dns_names = {{ {too_many} }},"),
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyCertificateDnsNames { certificate }
            if certificate == "web-certificate"
    ));

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = {},",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::EmptyCertificateDnsNames { certificate }
            if certificate == "web-certificate"
    ));

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"www.example.test\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificateDnsName {
            certificate,
            dns_name
        } if certificate == "web-certificate" && dns_name == "www.example.test"
    ));

    for dns_name in [
        "",
        "example.test.",
        "caf\u{e9}.example.test",
        "-api.example.test",
        "api-.example.test",
        "api..example.test",
        "api_example.test",
        "*",
        "api.*.example.test",
        "www*.example.test",
        "*.127.0.0.1",
    ] {
        let source = changed(
            "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
            &format!("      dns_names = {{ \"{dns_name}\" }},"),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidCertificateDnsName { certificate, .. }
                if certificate == "web-certificate"
        ));
    }
}

#[test]
fn validates_and_canonicalizes_certificate_ip_identities() {
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"192.0.2.10\", \"2001:0DB8:0:0:0:0:0:1\" },",
    );
    let config = load_lua(&source).expect("IPv4 and IPv6 certificate identities");
    assert_eq!(
        config.certificates[0].dns_names,
        ["192.0.2.10", "2001:db8::1"]
    );

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"2001:db8::1\", \"2001:0DB8:0:0:0:0:0:1\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificateDnsName { dns_name, .. }
            if dns_name == "2001:db8::1"
    ));

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"192.0.2.10\", \"::ffff:c000:020a\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificateDnsName { dns_name, .. }
            if dns_name == "192.0.2.10"
    ));
}

#[test]
fn validates_certificate_file_paths_lexically() {
    let path = format!("/{}", "a".repeat(4_095));
    let source = changed(
        "        certificate_chain_path = \"/etc/oxiroute/web-chain.pem\",",
        &format!("        certificate_chain_path = \"{path}\","),
    );
    load_lua(&source).expect("4096-byte path");

    let too_long = format!("/{}", "a".repeat(4_096));
    let source = changed(
        "        certificate_chain_path = \"/etc/oxiroute/web-chain.pem\",",
        &format!("        certificate_chain_path = \"{too_long}\","),
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::InvalidFilePath {
            kind: "certificate",
            field: "source.certificate_chain_path",
            ..
        }
    ));

    for path in [
        "etc/oxiroute/cert.pem",
        "/",
        "/etc//cert.pem",
        "/etc/certs/",
        "/etc/./cert.pem",
        "/etc/../cert.pem",
        r"/etc/\0cert.pem",
    ] {
        let source = changed(
            "        certificate_chain_path = \"/etc/oxiroute/web-chain.pem\",",
            &format!("        certificate_chain_path = \"{path}\","),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidFilePath {
                kind: "certificate",
                field: "source.certificate_chain_path",
                ..
            }
        ));
    }

    let source = changed(
        "        private_key_path = \"/etc/oxiroute/web-key.pem\",",
        "        private_key_path = \"/etc/oxiroute/web-chain.pem\",",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificatePaths { certificate }
            if certificate == "web-certificate"
    ));
}
