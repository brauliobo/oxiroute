use std::path::Path;

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_SOURCE_BYTES, decode_value, render_config, render_value,
};
use serde_json::json;

#[test]
fn infers_supported_extensions_and_kdl_default() {
    assert_eq!(ConfigFormat::default(), ConfigFormat::Kdl);
    assert_eq!(ConfigFormat::infer("config").unwrap(), ConfigFormat::Kdl);
    assert_eq!(
        ConfigFormat::infer("config.KDL2").unwrap(),
        ConfigFormat::Kdl
    );
    assert_eq!(
        ConfigFormat::infer("config.lua").unwrap(),
        ConfigFormat::Lua
    );
    assert_eq!(
        ConfigFormat::infer("config.uci").unwrap(),
        ConfigFormat::Uci
    );
    assert_eq!(
        ConfigFormat::infer("config.hocon").unwrap(),
        ConfigFormat::Hocon
    );
    assert_eq!(
        ConfigFormat::infer("config.conf").unwrap(),
        ConfigFormat::Hocon
    );
    assert!(ConfigFormat::infer(Path::new("config.toml")).is_err());
}

#[test]
fn lua_adapter_is_explicitly_unsupported() {
    assert!(matches!(
        decode_value(ConfigFormat::Lua, b"return {}"),
        Err(ConfigSourceError::UnsupportedAdapter {
            format: "Lua",
            operation: "decode"
        })
    ));
    assert!(matches!(
        render_value(ConfigFormat::Lua, &json!({})),
        Err(ConfigSourceError::UnsupportedAdapter {
            format: "Lua",
            operation: "render"
        })
    ));
}

#[test]
fn all_decoders_apply_the_source_bound_before_parsing() {
    let oversized = vec![b' '; MAX_SOURCE_BYTES + 1];
    for format in [ConfigFormat::Kdl, ConfigFormat::Uci, ConfigFormat::Hocon] {
        assert!(matches!(
            decode_value(format, &oversized),
            Err(ConfigSourceError::SourceTooLarge)
        ));
    }
}

#[test]
fn typed_configs_render_in_every_supported_format() {
    let config = oxiroute_config::Config {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    };

    for format in [
        ConfigFormat::Kdl,
        ConfigFormat::Lua,
        ConfigFormat::Uci,
        ConfigFormat::Hocon,
    ] {
        let rendered = render_config(format, &config).expect("typed render");
        let resolved = oxiroute_config_source::resolve_source_with_format(
            Path::new("config"),
            rendered.as_bytes(),
            format,
        )
        .expect("rendered source resolves");
        assert_eq!(resolved.config, config);
    }
}
