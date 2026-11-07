use std::path::Path;

use oxiroute_config_source::{ConfigFormat, render_config};
use serde_json::json;

fn canary_config() -> oxiroute_config::ValidatedConfig {
    serde_json::from_value::<oxiroute_config::ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "upstream_pools": [
            {"name": "farm", "servers": [
                {"name": "a", "endpoint": {"type": "socket", "address": "192.0.2.10:9000"}}
            ], "canary": {"path": "/healthz/deep?full=1", "host": "farm.example.com",
                          "expected_status": 204, "interval_ms": 15000, "timeout_ms": 2000,
                          "max_latency_ms": 500}},
            {"name": "plain", "servers": [
                {"name": "b", "endpoint": {"type": "socket", "address": "192.0.2.11:9000"}}
            ], "canary": {"path": "/"}},
            {"name": "none", "servers": [
                {"name": "c", "endpoint": {"type": "socket", "address": "192.0.2.12:9000"}}
            ]}
        ]
    }))
    .expect("config draft")
    .validate()
    .expect("canary config validates")
}

#[test]
fn canaries_round_trip_through_every_format() {
    let config = canary_config();
    let pools = &config.as_draft().upstream_pools;
    assert_eq!(
        pools[0].canary.as_ref().map(|c| c.expected_status),
        Some(204)
    );
    assert_eq!(
        pools[1].canary.as_ref().map(|c| c.interval_ms),
        Some(30_000)
    );
    assert!(pools[2].canary.is_none());
    for format in [
        ConfigFormat::Kdl,
        ConfigFormat::Lua,
        ConfigFormat::Uci,
        ConfigFormat::Hocon,
    ] {
        let rendered = render_config(format, &config).expect("render");
        let resolved = oxiroute_config_source::resolve_source_with_format(
            Path::new("config"),
            rendered.as_bytes(),
            format,
        )
        .unwrap_or_else(|error| panic!("{format:?}: {error:?}"));
        assert_eq!(resolved.config, config, "{format:?}");
    }
}
