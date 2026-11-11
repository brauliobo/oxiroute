mod lua_support;

use std::path::Path;

use lua_support::{load_lua, load_lua_error, render_lua};
use oxiroute_config::KtlsMode;
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};

const FORMATS: [ConfigFormat; 4] = [
    ConfigFormat::Kdl,
    ConfigFormat::Lua,
    ConfigFormat::Uci,
    ConfigFormat::Hocon,
];

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

fn resolve(
    format: ConfigFormat,
    rendered: &str,
) -> Result<(), oxiroute_config_source::ConfigSourceError> {
    resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format).map(|_| ())
}

#[test]
fn ktls_defaults_to_auto_and_renders_explicitly() {
    let config = load_lua(&config_source("{}")).expect("default ktls policy");
    assert_eq!(config.tls_profiles[0].policy.ktls, KtlsMode::Auto);
    let rendered = render_lua(&config).expect("rendered default ktls policy");
    assert!(rendered.contains("ktls = \"auto\","));
    assert_eq!(load_lua(&rendered).expect("default ktls reload"), config);
}

#[test]
fn ktls_off_round_trips_in_every_format() {
    let config = load_lua(&config_source(r#"{ ktls = "off" }"#)).expect("off ktls policy");
    assert_eq!(config.tls_profiles[0].policy.ktls, KtlsMode::Off);
    let validated = config.validate().expect("valid off ktls policy");
    for format in FORMATS {
        let rendered = render_config(format, &validated).expect("off ktls render");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .expect("rendered off ktls source resolves");
        assert_eq!(resolved.config, validated, "{format:?}");
    }
}

#[test]
fn ktls_auto_round_trips_in_every_format() {
    let config = load_lua(&config_source(r#"{ ktls = "auto" }"#)).expect("auto ktls policy");
    assert_eq!(config.tls_profiles[0].policy.ktls, KtlsMode::Auto);
    let validated = config.validate().expect("valid auto ktls policy");
    for format in FORMATS {
        let rendered = render_config(format, &validated).expect("auto ktls render");
        assert!(rendered.contains("auto"), "{format:?}: {rendered}");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .expect("rendered auto ktls source resolves");
        assert_eq!(resolved.config, validated, "{format:?}");
    }
}

#[test]
fn ktls_rejects_unknown_values_in_every_format() {
    let error = load_lua_error(&config_source(r#"{ ktls = "static" }"#)).to_string();
    assert!(error.contains("unknown variant"), "{error}");

    let validated = load_lua(&config_source(r#"{ ktls = "auto" }"#))
        .expect("auto ktls policy")
        .validate()
        .expect("valid auto ktls policy");
    for format in [ConfigFormat::Kdl, ConfigFormat::Uci, ConfigFormat::Hocon] {
        let rendered = render_config(format, &validated).expect("auto ktls render");
        let error =
            resolve(format, &rendered.replace("auto", "static")).expect_err("unknown ktls mode");
        assert!(
            error.to_string().contains("unknown variant"),
            "{format:?}: {error}"
        );
    }
}
