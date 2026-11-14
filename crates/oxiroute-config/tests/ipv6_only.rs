use oxiroute_config::{ConfigDraft, ConfigError, ListenerBind, ValidatedConfig};
use serde_json::{Value, json};

fn socket(address: &str, ipv6_only: bool) -> Value {
    let mut bind = json!({"type": "socket", "address": address});
    if ipv6_only {
        bind["ipv6_only"] = json!(true);
    }
    bind
}

fn validate(binds: &[Value]) -> Result<ValidatedConfig, ConfigError> {
    let listeners: Vec<Value> = binds
        .iter()
        .enumerate()
        .map(|(index, bind)| {
            json!({"name": format!("edge-{index}"), "bind": bind, "protocol": "http", "service": "web"})
        })
        .collect();
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "upstream_pools": [{"name": "app", "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]}],
        "http_services": [{"name": "web", "routes": [{
            "path": {"kind": "exact", "value": "/"},
            "action": {"type": "proxy", "upstream_pool": "app", "policy": {}}}]}],
        "listeners": listeners,
    }))
    .expect("authored configuration")
    .validate()
}

#[test]
fn ipv6_only_is_omitted_from_the_canonical_form_unless_set() {
    let dual = validate(&[socket("[::]:8080", false)]).unwrap();
    assert!(
        serde_json::to_value(&dual).unwrap()["listeners"][0]["bind"]
            .get("ipv6_only")
            .is_none()
    );
    let only = validate(&[socket("[::]:8080", true)]).unwrap();
    assert_eq!(
        only.as_draft().listeners[0].bind,
        ListenerBind::Socket {
            address: "[::]:8080".parse().unwrap(),
            ipv6_only: true
        }
    );
    assert_eq!(
        serde_json::to_value(&only).unwrap()["listeners"][0]["bind"]["ipv6_only"],
        json!(true)
    );
}

#[test]
fn ipv6_only_is_refused_on_anything_but_the_ipv6_wildcard() {
    for address in ["0.0.0.0:8080", "127.0.0.1:8080", "[::1]:8080"] {
        let error = validate(&[socket(address, true)]).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidListenerTransport { .. }),
            "{address}: {error}"
        );
    }
}

#[test]
fn an_ipv6_only_wildcard_shares_its_port_with_ipv4_listeners_only() {
    validate(&[socket("0.0.0.0:8080", false), socket("[::]:8080", true)]).unwrap();
    validate(&[socket("[::]:8080", true), socket("127.0.0.1:8080", false)]).unwrap();
    for pair in [
        [socket("0.0.0.0:8080", false), socket("[::]:8080", false)],
        [socket("[::]:8080", true), socket("[::]:8080", true)],
        [socket("[::]:8080", true), socket("[::1]:8080", false)],
        [socket("[::]:8080", false), socket("127.0.0.1:8080", false)],
    ] {
        assert!(
            matches!(
                validate(&pair).unwrap_err(),
                ConfigError::OverlappingBind { .. }
            ),
            "{pair:?}"
        );
    }
}
