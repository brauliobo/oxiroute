use oxiroute_config::{ConfigDraft, ConfigError, PoolCanary};
use serde_json::{Value, json};

fn config(canary: &Value, extra: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    let mut pool = json!({
        "name": "farm",
        "servers": [{"name": "a", "endpoint": {"type": "socket", "address": "192.0.2.10:9000"}}],
        "canary": canary,
    });
    pool.as_object_mut()
        .expect("pool")
        .extend(extra.as_object().expect("extra").clone());
    serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "upstream_pools": [pool]}),
    )
    .expect("draft")
    .validate()
}

#[test]
fn a_canary_needs_only_a_path_and_defaults_the_rest() {
    let validated = config(&json!({"path": "/healthz/deep"}), &json!({})).expect("valid");
    let canary = validated.as_draft().upstream_pools[0]
        .canary
        .clone()
        .expect("canary");
    assert_eq!(
        canary,
        PoolCanary {
            path: "/healthz/deep".into(),
            host: None,
            expected_status: 200,
            interval_ms: 30_000,
            timeout_ms: 5_000,
            max_latency_ms: None,
        }
    );
    let rendered = serde_json::to_value(validated.as_draft()).expect("json");
    assert_eq!(
        rendered["upstream_pools"][0]["canary"]["interval_ms"],
        30_000
    );
}

#[test]
fn a_pool_without_a_canary_serializes_without_the_key() {
    let validated = config(&Value::Null, &json!({})).expect("valid");
    let rendered = serde_json::to_value(validated.as_draft()).expect("json");
    assert!(rendered["upstream_pools"][0].get("canary").is_none());
}

#[test]
fn canary_bounds_are_validated_with_field_names() {
    for (canary, field) in [
        (json!({"path": "healthz"}), "canary.path"),
        (json!({"path": "/a b"}), "canary.path"),
        (json!({"path": "/x", "host": ""}), "canary.host"),
        (
            json!({"path": "/x", "expected_status": 99}),
            "canary.expected_status",
        ),
        (
            json!({"path": "/x", "expected_status": 600}),
            "canary.expected_status",
        ),
        (
            json!({"path": "/x", "interval_ms": 999}),
            "canary.interval_ms",
        ),
        (json!({"path": "/x", "timeout_ms": 50}), "canary.timeout_ms"),
        (
            json!({"path": "/x", "interval_ms": 2000, "timeout_ms": 3000}),
            "canary.timeout_ms",
        ),
        (
            json!({"path": "/x", "max_latency_ms": 0}),
            "canary.max_latency_ms",
        ),
        (
            json!({"path": "/x", "timeout_ms": 1000, "max_latency_ms": 2000}),
            "canary.max_latency_ms",
        ),
    ] {
        let error = config(&canary, &json!({})).expect_err("invalid canary");
        let pointer = error.pointer().expect("pointer");
        assert!(pointer.ends_with(field), "{canary}: {pointer}");
    }
}

#[test]
fn a_canary_speaks_plain_http_to_socket_servers_like_health_checks_do() {
    let tls = json!({"tls": {"server_name": "backend.example.com"}});
    let error = config(&json!({"path": "/x"}), &tls).expect_err("tls is not supported");
    assert!(error.pointer().expect("pointer").ends_with("canary"));
    let unix =
        json!({"servers": [{"name": "a", "endpoint": {"type": "unix", "path": "/run/a.sock"}}]});
    let error = config(&json!({"path": "/x"}), &unix).expect_err("unix is not supported");
    assert!(error.pointer().expect("pointer").ends_with("canary"));
}
