use std::path::Path;

use oxiroute_config::{ConfigDraft, ListenerBind, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::json;

const FORMATS: [ConfigFormat; 4] = [
    ConfigFormat::Kdl,
    ConfigFormat::Lua,
    ConfigFormat::Uci,
    ConfigFormat::Hocon,
];

fn validated(ipv6_only: bool) -> ValidatedConfig {
    let v4_port = if ipv6_only { 8080 } else { 8081 };
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "upstream_pools": [{"name": "app", "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]}],
        "http_services": [{"name": "web", "routes": [{
            "path": {"kind": "exact", "value": "/"},
            "action": {"type": "proxy", "upstream_pool": "app", "policy": {}}}]}],
        "listeners": [
            {"name": "v4", "bind": {"type": "socket", "address": format!("0.0.0.0:{v4_port}")}, "protocol": "http", "service": "web"},
            {"name": "v6", "bind": {"type": "socket", "address": "[::]:8080", "ipv6_only": ipv6_only},
             "protocol": "http", "service": "web"},
        ],
    }))
    .expect("authored configuration")
    .validate()
    .expect("valid configuration")
}

#[test]
fn ipv6_only_round_trips_through_every_source_format() {
    let config = validated(true);
    assert!(matches!(
        config.as_draft().listeners[1].bind,
        ListenerBind::Socket {
            ipv6_only: true,
            ..
        }
    ));
    for format in FORMATS {
        let rendered = render_config(format, &config).expect("render");
        assert!(rendered.contains("ipv6_only"), "{format:?}\n{rendered}");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|error| panic!("{format:?}: {error}\n{rendered}"));
        assert_eq!(resolved.config, config, "{format:?}\n{rendered}");
    }
}

#[test]
fn dual_stack_is_the_default_and_is_never_rendered() {
    let config = validated(false);
    for format in FORMATS {
        let rendered = render_config(format, &config).expect("render");
        assert!(!rendered.contains("ipv6_only"), "{format:?}\n{rendered}");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|error| panic!("{format:?}: {error}\n{rendered}"));
        assert_eq!(resolved.config, config, "{format:?}");
    }
}
