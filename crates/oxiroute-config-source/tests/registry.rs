use std::path::Path;

use oxiroute_config_source::{ConfigFormat, render_config};
use serde_json::json;

fn registering_config() -> oxiroute_config::ValidatedConfig {
    serde_json::from_value::<oxiroute_config::ConfigDraft>(json!({
        "version": 1,
        "registry": {
            "state_dir": "/var/lib/oxiroute/registry",
            "tokens_file": "/etc/oxiroute/registry.tokens",
            "endpoints": [
                {"type": "unix", "path": "/run/oxiroute/status.sock", "mode": 288, "role": "read"},
                {"type": "unix", "path": "/run/oxiroute/register.sock", "mode": 432,
                 "role": "register", "pools": ["render"]},
                {"type": "tcp", "bind": "192.0.2.5:9911", "allow_cidrs": ["192.0.2.0/24"]}
            ]
        },
        "listeners": [],
        "upstream_pools": [
            {"name": "render", "servers": [], "algorithm": "first",
             "registration": {"max_servers": 8, "allowed_cidrs": ["192.0.2.0/24"]}},
            {"name": "voice", "servers": [
                {"name": "local", "endpoint": {"type": "socket", "address": "127.0.0.1:9000"}}
            ], "registration": {"require_peer_address": false}}
        ]
    }))
    .expect("config draft")
    .validate()
    .expect("registering config validates")
}

#[test]
fn registration_and_registry_round_trip_through_every_format() {
    let config = registering_config();
    let render = &config.as_draft().upstream_pools[0];
    assert_eq!(render.servers, Vec::new());
    assert_eq!(
        render
            .registration
            .as_ref()
            .expect("registration")
            .max_servers,
        8
    );
    assert_eq!(
        render
            .registration
            .as_ref()
            .expect("registration")
            .default_ttl_ms,
        15_000
    );

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
