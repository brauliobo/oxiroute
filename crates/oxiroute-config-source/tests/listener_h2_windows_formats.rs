use std::path::Path;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::{Value, json};

fn validated(listener_windows: &Value) -> ValidatedConfig {
    let mut listener = json!({
        "name": "edge",
        "bind": {"type": "socket", "address": "127.0.0.1:8080"},
        "protocol": "http",
        "service": "web",
    });
    listener
        .as_object_mut()
        .unwrap()
        .extend(listener_windows.as_object().unwrap().clone());
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "upstream_pools": [{"name": "app", "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]}],
        "http_services": [{"name": "web", "routes": [{
            "path": {"kind": "exact", "value": "/"},
            "action": {"type": "proxy", "upstream_pool": "app", "policy": {}}}]}],
        "listeners": [listener],
    }))
    .expect("authored configuration")
    .validate()
    .expect("valid configuration")
}

fn round_trip(config: &ValidatedConfig) {
    for format in [
        ConfigFormat::Kdl,
        ConfigFormat::Lua,
        ConfigFormat::Uci,
        ConfigFormat::Hocon,
    ] {
        let rendered = render_config(format, config).expect("render");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|error| panic!("{format:?}: {error}\n{rendered}"));
        assert_eq!(&resolved.config, config, "{format:?}\n{rendered}");
    }
}

#[test]
fn both_windows_round_trip_through_every_source_format() {
    let config = validated(&json!({
        "h2_initial_stream_window": 262_144, "h2_initial_connection_window": 2_097_152}));
    let listener = &config.as_draft().listeners[0];
    assert_eq!(
        (
            listener.h2_initial_stream_window,
            listener.h2_initial_connection_window
        ),
        (Some(262_144), Some(2_097_152))
    );
    round_trip(&config);
}

#[test]
fn one_window_and_none_round_trip_through_every_source_format() {
    round_trip(&validated(
        &json!({"h2_initial_connection_window": 4_194_304}),
    ));
    round_trip(&validated(&json!({"h2_initial_stream_window": 65_535})));
    let none = validated(&json!({}));
    round_trip(&none);
    for format in [
        ConfigFormat::Kdl,
        ConfigFormat::Lua,
        ConfigFormat::Uci,
        ConfigFormat::Hocon,
    ] {
        let rendered = render_config(format, &none).expect("render");
        assert!(!rendered.contains("h2_initial"), "{format:?}\n{rendered}");
    }
}
