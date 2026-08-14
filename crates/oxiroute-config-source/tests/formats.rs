use std::path::Path;

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_SOURCE_BYTES, decode_value, load_lua, render_config,
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
    let config = oxiroute_config::ConfigDraft {
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
    }
    .validate()
    .expect("valid typed config");

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

#[test]
fn h3_cache_round_trips_through_validated_lua() {
    let draft: oxiroute_config::ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "certificates": [{
            "name": "forward-cert",
            "dns_names": ["proxy.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/etc/oxiroute/forward-chain.pem",
                "private_key_path": "/etc/oxiroute/forward-key.pem"
            }
        }],
        "tls_profiles": [{
            "name": "forward",
            "certificates": ["forward-cert"],
            "default_certificate": "forward-cert",
            "min_version": "1.3",
            "alpn": ["h3"]
        }],
        "listeners": [{
            "name": "forward-http3",
            "bind": {"type": "udp", "address": "127.0.0.1:8443"},
            "protocol": "forward_http3",
            "service": "egress",
            "tls_profile": "forward"
        }],
        "cache_stores": [{"name": "memory", "type": "memory"}],
        "forward_proxy_services": [{
            "name": "egress",
            "enabled_versions": ["h1", "h2", "h3"],
            "cache": {"store": "memory"}
        }]
    }))
    .expect("authored H3 cache configuration");

    let validated = draft.validate().expect("validated H3 cache configuration");
    let lua = render_config(ConfigFormat::Lua, &validated).expect("validated H3 Lua rendering");
    let loaded = load_lua(&lua).expect("validated H3 Lua loading");

    assert_eq!(loaded, validated);
}
