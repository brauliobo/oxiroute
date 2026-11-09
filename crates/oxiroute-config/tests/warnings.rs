use oxiroute_config::{
    ConfigDraft, ValidatedConfig, W_PASSIVE_HEALTH_MISSING, W_RETRY_NEVER_REPLAYS,
};
use serde_json::{Value, json};

const STATUS_RETRY: &str = r#"{"max_retries": 2, "triggers": [], "response_statuses": [500]}"#;

fn pool(name: &str, passive_health: bool) -> Value {
    let mut pool = json!({
        "name": name,
        "endpoints": [{"type": "socket", "address": "127.0.0.1:3000"}]
    });
    if passive_health {
        pool["passive_health"] = json!({});
    }
    pool
}

fn route(methods: &[&str], policy: &Value) -> Value {
    json!({
        "path": {"kind": "exact", "value": "/"},
        "methods": methods,
        "action": {"type": "proxy", "upstream_pool": "origin", "policy": policy}
    })
}

fn retrying(retry: &str, methods: &[&str]) -> Value {
    route(
        methods,
        &json!({"retry": serde_json::from_str::<Value>(retry).unwrap()}),
    )
}

fn config(pools: &[Value], routes: &[Value]) -> ValidatedConfig {
    let draft: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "upstream_pools": pools,
        "http_services": [{"name": "web", "routes": routes}]
    }))
    .expect("authored configuration");
    draft.validate().expect("valid configuration")
}

fn codes(routes: &[Value]) -> Vec<&'static str> {
    let warnings = config(&[pool("origin", true)], routes).warnings();
    warnings.iter().map(|warning| warning.code).collect()
}

#[test]
fn response_retries_on_routes_that_accept_no_replayable_method_warn() {
    let warned = config(
        &[pool("origin", true)],
        &[retrying(STATUS_RETRY, &["POST"])],
    )
    .warnings();
    assert_eq!(warned.len(), 1);
    assert_eq!(warned[0].code, W_RETRY_NEVER_REPLAYS);
    assert!(warned[0].to_string().contains("route 0"));

    let response_trigger = r#"{"max_retries": 1, "triggers": ["response_timeout"]}"#;
    assert_eq!(
        codes(&[retrying(response_trigger, &["post", "PUT"])]),
        [W_RETRY_NEVER_REPLAYS]
    );

    let rescue = route(
        &["POST"],
        &json!({"retry": {"max_retries": 1}, "rescue": {"statuses": [404]}}),
    );
    assert_eq!(codes(&[rescue]), [W_RETRY_NEVER_REPLAYS]);
}

#[test]
fn routes_that_replay_some_request_never_warn() {
    for methods in [&[][..], &["GET"], &["head"], &["POST", "GET"]] {
        assert!(codes(&[retrying(STATUS_RETRY, methods)]).is_empty());
    }

    let buffered = r#"{"max_retries": 1, "triggers": [], "response_statuses": [500],
        "method_safety": "all", "body_safety": "buffered"}"#;
    let mut post = retrying(buffered, &["POST"]);
    post["policy"] = json!({"request_buffering": true});
    assert!(codes(&[post]).is_empty());
}

#[test]
fn connect_retries_and_defaults_never_warn() {
    let connect = r#"{"max_retries": 2, "triggers": ["connect_failure", "connect_timeout"]}"#;
    assert!(codes(&[retrying(connect, &["POST"])]).is_empty());
    assert!(codes(&[retrying(r#"{"max_retries": 2}"#, &["POST"])]).is_empty());
    assert!(codes(&[retrying("{}", &["POST"])]).is_empty());
}

#[test]
fn status_retry_pools_without_passive_health_warn_once() {
    let routes = [
        retrying(STATUS_RETRY, &[]),
        retrying(STATUS_RETRY, &["GET"]),
    ];
    let warned = config(&[pool("origin", false)], &routes).warnings();
    assert_eq!(warned.len(), 1);
    assert_eq!(warned[0].code, W_PASSIVE_HEALTH_MISSING);
    assert!(warned[0].message.contains("`origin`"));

    assert!(
        config(&[pool("origin", true)], &routes)
            .warnings()
            .is_empty()
    );
}

#[test]
fn pools_that_retry_no_status_need_no_passive_health() {
    let connect = r#"{"max_retries": 2}"#;
    let routes = [retrying(connect, &[])];
    assert!(
        config(&[pool("origin", false)], &routes)
            .warnings()
            .is_empty()
    );
}

#[test]
fn selected_pools_serve_the_status_retries_of_the_route() {
    let mut selecting = retrying(STATUS_RETRY, &[]);
    selecting["action"]["policy"]["select"] = json!({
        "header": "x-size",
        "rules": [{"at_most": 10, "pool": "small"}]
    });
    let warned = config(&[pool("origin", true), pool("small", false)], &[selecting]).warnings();
    assert_eq!(warned.len(), 1);
    assert!(warned[0].message.contains("`small`"));
}
