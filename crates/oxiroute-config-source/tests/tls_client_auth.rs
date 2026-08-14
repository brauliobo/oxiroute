mod lua_support;

use lua_support::{load_lua, render_lua};
use oxiroute_config::{ConfigError, TlsClientAuthMode};

fn config_source(policy: &str) -> String {
    format!(
        r#"return {{
  version = 1,
  certificates = {{
    {{
      name = "server",
      dns_names = {{ "server.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/server-chain.pem",
        private_key_path = "/etc/oxiroute/server-key.pem",
      }},
    }},
  }},
  tls_profiles = {{
    {{
      name = "web",
      certificates = {{ "server" }},
      default_certificate = "server",
      policy = {policy},
    }},
  }},
  listeners = {{}},
}}"#
    )
}

#[test]
fn client_auth_defaults_to_disabled_and_renders_explicitly() {
    let config = load_lua(&config_source("{}")).expect("default client auth policy");
    assert_eq!(
        config.tls_profiles[0].policy.client_auth.mode,
        TlsClientAuthMode::Disabled
    );
    let rendered = render_lua(&config).expect("rendered client auth policy");
    assert!(rendered.contains("client_auth = {"));
    assert!(rendered.contains("mode = \"disabled\","));
    assert_eq!(load_lua(&rendered).expect("rendered policy reload"), config);
}

#[test]
fn client_auth_normalizes_allowed_dns_names_and_supports_each_mode() {
    for mode in ["optional", "required"] {
        let source = config_source(&format!(
            r#"{{
        client_auth = {{
          mode = "{mode}",
          ca_certificate_path = "/etc/oxiroute/client-ca.pem",
          allowed_dns_names = {{ "CLIENT.EXAMPLE.TEST", "192.0.2.7" }},
        }},
      }}"#
        ));
        let config = load_lua(&source).expect("enabled client auth policy");
        assert_eq!(
            match config.tls_profiles[0].policy.client_auth.mode {
                TlsClientAuthMode::Optional => "optional",
                TlsClientAuthMode::Required => "required",
                TlsClientAuthMode::Disabled => "disabled",
            },
            mode
        );
        assert_eq!(
            config.tls_profiles[0].policy.client_auth.allowed_dns_names,
            ["client.example.test", "192.0.2.7"]
        );
        let rendered = render_lua(&config).expect("rendered enabled policy");
        assert_eq!(load_lua(&rendered).expect("enabled policy reload"), config);
    }
}

#[test]
fn client_auth_rejects_missing_ca_and_disabled_policy_material() {
    let missing_ca = config_source(
        r#"{
          client_auth = { mode = "required" },
        }"#,
    );
    assert!(matches!(
        load_lua(&missing_ca),
        Err(ConfigError::InvalidTlsProfilePolicy {
            field: "policy.client_auth.ca_certificate_path",
            ..
        })
    ));

    let disabled_ca = config_source(
        r#"{
          client_auth = {
            mode = "disabled",
            ca_certificate_path = "/etc/oxiroute/client-ca.pem",
          },
        }"#,
    );
    assert!(matches!(
        load_lua(&disabled_ca),
        Err(ConfigError::InvalidTlsProfilePolicy {
            field: "policy.client_auth.ca_certificate_path",
            ..
        })
    ));
}

#[test]
fn client_auth_rejects_wildcard_and_duplicate_identity_rules() {
    for names in [
        r#"{ "*.clients.example.test" }"#,
        r#"{ "CLIENT.EXAMPLE.TEST", "client.example.test" }"#,
    ] {
        let source = config_source(&format!(
            r#"{{
        client_auth = {{
          mode = "optional",
          ca_certificate_path = "/etc/oxiroute/client-ca.pem",
          allowed_dns_names = {names},
        }},
      }}"#
        ));
        assert!(matches!(
            load_lua(&source),
            Err(ConfigError::InvalidTlsClientAuthDnsName { .. }
                | ConfigError::DuplicateTlsClientAuthDnsName { .. },)
        ));
    }
}
