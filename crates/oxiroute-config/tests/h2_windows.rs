use oxiroute_config::{ConfigDraft, ConfigError, Listener, ValidatedConfig};
use serde_json::{Value, json};

/// One listener of `protocol` (`http` serves `web`, the forward protocols serve `egress`) with `extra` fields.
fn validate(protocol: &str, extra: &Value) -> Result<ValidatedConfig, ConfigError> {
    let mut listener = json!({
        "name": "edge",
        "bind": {"type": "socket", "address": "127.0.0.1:8080"},
        "protocol": protocol,
        "service": if protocol == "http" { "web" } else { "egress" },
    });
    listener
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let config = json!({
        "version": 1,
        "upstream_pools": [{"name": "app", "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]}],
        "http_services": [{"name": "web", "routes": [{
            "path": {"kind": "exact", "value": "/"},
            "action": {"type": "proxy", "upstream_pool": "app", "policy": {}}}]}],
        "forward_proxy_services": [{"name": "egress", "enabled_versions": ["h1", "h2"], "tls_required": false}],
        "listeners": [listener],
    });
    serde_json::from_value::<ConfigDraft>(config)
        .expect("authored configuration")
        .validate()
}

fn effective(protocol: &str, extra: &Value) -> (u32, u32) {
    let config = validate(protocol, extra).unwrap();
    let windows = config.as_draft().listeners[0].effective_h2_windows();
    (windows.stream, windows.connection)
}

fn refusal(protocol: &str, extra: &Value) -> ConfigError {
    validate(protocol, extra).unwrap_err()
}

#[test]
fn absence_advertises_one_and_eight_mib_and_stays_out_of_the_canonical_form() {
    assert_eq!(effective("http", &json!({})), (1 << 20, 8 << 20));
    let config = validate("http", &json!({})).unwrap();
    let canonical = serde_json::to_value(&config).unwrap();
    let listener = &canonical["listeners"][0];
    assert!(listener.get("h2_initial_stream_window").is_none());
    assert!(listener.get("h2_initial_connection_window").is_none());
}

#[test]
fn values_are_taken_as_written_and_may_be_set_one_at_a_time() {
    let both =
        json!({"h2_initial_stream_window": 262_144, "h2_initial_connection_window": 2_097_152});
    assert_eq!(effective("http", &both), (262_144, 2_097_152));
    let stream = json!({"h2_initial_stream_window": 65_535});
    assert_eq!(effective("http", &stream), (65_535, 8 << 20));
    let connection = json!({"h2_initial_connection_window": 4 << 20});
    assert_eq!(effective("http", &connection), (1 << 20, 4 << 20));
}

#[test]
fn both_bounds_of_each_window_are_accepted() {
    let low = json!({"h2_initial_stream_window": Listener::MIN_H2_WINDOW,
        "h2_initial_connection_window": Listener::MIN_H2_WINDOW});
    assert_eq!(effective("http", &low), (65_535, 65_535));
    let high = json!({"h2_initial_stream_window": Listener::MAX_H2_STREAM_WINDOW,
        "h2_initial_connection_window": Listener::MAX_H2_CONNECTION_WINDOW});
    assert_eq!(effective("http", &high), (16 << 20, 64 << 20));
}

#[test]
fn a_window_outside_its_range_is_refused_with_its_field() {
    for (field, value) in [
        ("h2_initial_stream_window", 65_534),
        (
            "h2_initial_stream_window",
            Listener::MAX_H2_STREAM_WINDOW + 1,
        ),
        ("h2_initial_connection_window", 0),
        (
            "h2_initial_connection_window",
            Listener::MAX_H2_CONNECTION_WINDOW + 1,
        ),
    ] {
        let error = refusal("http", &json!({field: value}));
        assert!(
            matches!(&error, ConfigError::InvalidListenerH2Window { field: f, value: v, .. }
                if *f == field && *v == value),
            "{field} {value}: {error}"
        );
        assert_eq!(error.pointer().unwrap(), format!("/listeners/edge/{field}"));
    }
}

#[test]
fn a_connection_window_below_the_stream_window_is_refused() {
    let error = refusal("http", &json!({"h2_initial_stream_window": 16 << 20}));
    assert!(
        matches!(error, ConfigError::ListenerH2ConnectionWindowBelowStream { stream, connection, .. }
            if (stream, connection) == (16 << 20, 8 << 20)),
        "{error}"
    );
    let both =
        json!({"h2_initial_stream_window": 2 << 20, "h2_initial_connection_window": 1 << 20});
    assert!(matches!(
        refusal("http", &both),
        ConfigError::ListenerH2ConnectionWindowBelowStream { .. }
    ));
}

#[test]
fn forward_http2_listeners_take_windows_and_other_protocols_refuse_them() {
    let stream = json!({"h2_initial_stream_window": 262_144});
    assert_eq!(effective("forward_http2", &stream), (262_144, 8 << 20));
    let error = refusal("forward_http1", &stream);
    assert!(
        matches!(error, ConfigError::ListenerH2WindowsUnsupported { .. }),
        "{error}"
    );
}
