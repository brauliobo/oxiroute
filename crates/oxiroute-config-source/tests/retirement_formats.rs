use std::path::Path;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::json;

fn validated() -> ValidatedConfig {
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "retirement": {"drain_deadline_ms": 90_000, "max_retired_generations": 2},
        "http_services": [{
            "name": "render",
            "drain_deadline_ms": 7_200_000,
            "routes": [{
                "path": {"kind": "exact", "value": "/"},
                "action": {"type": "fixed_response", "status": 200, "body": "ok"}
            }]
        }],
        "forward_proxy_services": [{"name": "egress", "drain_deadline_ms": 120_000}],
        "l4_services": [{"name": "relay", "upstream_pool": "p", "drain_deadline_ms": 60_000}],
        "upstream_pools": [{"name": "p", "servers": [{
            "name": "a", "endpoint": {"type": "socket", "address": "127.0.0.1:9"}
        }]}],
    }))
    .expect("authored retirement configuration")
    .validate()
    .expect("valid retirement configuration")
}

#[test]
fn retirement_keys_round_trip_through_every_source_format() {
    let config = validated();
    assert_eq!(config.retirement_deadline_ms(), 7_200_000);
    for format in [
        ConfigFormat::Kdl,
        ConfigFormat::Lua,
        ConfigFormat::Uci,
        ConfigFormat::Hocon,
    ] {
        let rendered = render_config(format, &config).expect("render");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|error| panic!("{format:?}: {error}"));
        assert_eq!(resolved.config, config, "{format:?}");
    }
}
