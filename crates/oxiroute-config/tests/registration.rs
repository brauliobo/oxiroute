use oxiroute_config::{ConfigDraft, ConfigError, PoolRegistration, cidr_contains};
use serde_json::{Value, json};

fn config(pool: &Value, registry: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "registry": registry,
        "listeners": [],
        "upstream_pools": [pool]
    }))
    .expect("draft")
    .validate()
}

fn registry() -> Value {
    json!({"state_dir": "/var/lib/oxiroute/registry"})
}

fn pool(servers: &Value, registration: &Value) -> Value {
    json!({"name": "render", "servers": servers, "registration": registration})
}

#[test]
fn only_a_registration_pool_may_start_without_servers() {
    assert!(config(&pool(&json!([]), &json!({})), &registry()).is_ok());
    assert!(matches!(
        config(&json!({"name": "render", "servers": []}), &registry()),
        Err(ConfigError::EmptyUpstreamEndpoints { .. })
    ));
}

#[test]
fn registration_needs_the_registry_block() {
    assert!(matches!(
        config(&pool(&json!([]), &json!({})), &Value::Null),
        Err(ConfigError::InvalidRegistry {
            field: "state_dir",
            ..
        })
    ));
}

#[test]
fn registration_bounds_are_validated_with_field_names() {
    for registration in [
        json!({"max_servers": 0}),
        json!({"min_ttl_ms": 0}),
        json!({"min_ttl_ms": 20000}),
        json!({"max_ttl_ms": 1000}),
        json!({"allowed_cidrs": ["192.0.2.0"]}),
        json!({"allowed_cidrs": ["192.0.2.0/33"]}),
    ] {
        let error = config(&pool(&json!([]), &registration), &registry()).expect_err("invalid");
        let pointer = error.pointer().expect("pointer");
        assert!(
            pointer.starts_with("/upstream_pools/render/servers/<pool>/registration"),
            "{registration}: {pointer}"
        );
    }
}

#[test]
fn registry_endpoints_are_checked() {
    let with = |endpoints: Value, tokens: Value| {
        config(
            &pool(&json!([]), &json!({})),
            &json!({"state_dir": "/s", "tokens_file": tokens, "endpoints": endpoints}),
        )
    };
    let unix =
        |path: &str, mode: u16| json!({"type": "unix", "path": path, "mode": mode, "role": "read"});

    assert!(with(json!([unix("/run/x.sock", 0o660)]), Value::Null).is_ok());
    assert!(with(json!([unix("run/x.sock", 0o660)]), Value::Null).is_err());
    assert!(with(json!([unix("/run/x.sock", 0o666)]), Value::Null).is_err());
    let tcp = json!([{"type": "tcp", "bind": "192.0.2.5:9911"}]);
    assert!(with(tcp.clone(), Value::Null).is_err());
    assert!(with(tcp, json!("/etc/oxiroute/tokens")).is_ok());
}

#[test]
fn cidr_matching_covers_both_families_and_rejects_malformed_entries() {
    let ip = |text: &str| text.parse().expect("ip");
    assert_eq!(cidr_contains("192.0.2.0/24", ip("192.0.2.77")), Some(true));
    assert_eq!(cidr_contains("192.0.2.0/24", ip("192.0.3.1")), Some(false));
    assert_eq!(cidr_contains("0.0.0.0/0", ip("203.0.113.1")), Some(true));
    assert_eq!(cidr_contains("192.0.2.1/32", ip("192.0.2.1")), Some(true));
    assert_eq!(
        cidr_contains("2001:db8::/32", ip("2001:db8::5")),
        Some(true)
    );
    assert_eq!(cidr_contains("2001:db8::/32", ip("192.0.2.1")), Some(false));
    assert_eq!(cidr_contains("::/0", ip("2001:db8::1")), Some(true));
    assert_eq!(cidr_contains("192.0.2.0/33", ip("192.0.2.1")), None);
    assert_eq!(cidr_contains("nonsense", ip("192.0.2.1")), None);
}

#[test]
fn defaults_match_the_documented_policy() {
    let policy = PoolRegistration::default();
    assert_eq!(
        (
            policy.max_servers,
            policy.min_ttl_ms,
            policy.default_ttl_ms,
            policy.max_ttl_ms
        ),
        (64, 2_000, 15_000, 3_600_000)
    );
    assert!(policy.require_peer_address && policy.allowed_cidrs.is_empty());
}
