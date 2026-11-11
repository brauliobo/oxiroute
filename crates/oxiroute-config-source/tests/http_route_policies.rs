mod http_routes_support;
mod lua_support;

use http_routes_support::{config, error, first_route, proxy_route};
use lua_support::{load_lua, load_lua_error, render_lua};
use oxiroute_config::{ConfigError, HttpRouteAction};
use serde_json::json;

#[test]
fn validates_proxy_timeouts_and_roundtrips_them() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let route = proxy_route(
        "",
        "              timeouts = { connect_ms = 500, header_ms = 2000, idle_ms = 1500, total_ms = 9000 },",
    );
    let loaded = load_lua(&config(&route, endpoint)).expect("proxy timeouts");
    let timeouts = &first_route(&config(&route, endpoint))["action"]["policy"]["timeouts"];
    assert_eq!(timeouts["header_ms"], 2000);
    assert_eq!(timeouts["total_ms"], 9000);
    let rendered = render_lua(&loaded).expect("render proxy timeouts");
    assert_eq!(load_lua(&rendered).expect("timeouts roundtrip"), loaded);

    let defaults = first_route(&config(&proxy_route("", ""), endpoint));
    assert_eq!(
        defaults["action"]["policy"]["timeouts"],
        json!({ "connect_ms": null, "header_ms": null, "idle_ms": null, "total_ms": null })
    );

    for policy in [
        "              timeouts = { header_ms = 0 },",
        "              timeouts = { idle_ms = 86400001 },",
        "              timeouts = { header_ms = 5000, total_ms = 1000 },",
        "              timeouts = { read_ms = 1000 },",
    ] {
        let error = error(&config(&proxy_route("", policy), endpoint));
        assert!(
            error.contains("timeouts") || error.contains("unknown field"),
            "{error}"
        );
    }
}

#[test]
fn validates_retry_budget_jitter_and_idempotency_header() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let route = proxy_route(
        "          policy = { request_buffering = true },",
        r#"              retry = {
                max_retries = 2,
                delay_ms = 100,
                jitter_percent = 50,
                method_safety = "all",
                body_safety = "buffered",
                idempotency_header = "Idempotency-Key",
                budget = { per_second = 5, ratio_percent = 20 },
              },"#,
    );
    let loaded = load_lua(&config(&route, endpoint)).expect("retry hardening");
    let retry = &first_route(&config(&route, endpoint))["action"]["policy"]["retry"];
    assert_eq!(retry["jitter_percent"], 50);
    assert_eq!(retry["idempotency_header"], "idempotency-key");
    assert_eq!(
        retry["budget"],
        json!({ "per_second": 5, "ratio_percent": 20 })
    );
    let rendered = render_lua(&loaded).expect("render retry hardening");
    assert_eq!(
        load_lua(&rendered).expect("retry hardening roundtrip"),
        loaded
    );

    for (policy, expected) in [
        ("retry = { jitter_percent = 101 },", "jitter_percent"),
        (
            "retry = { idempotency_header = \"x-key\" },",
            "idempotency_header",
        ),
        (
            "retry = { method_safety = \"all\", body_safety = \"buffered\", idempotency_header = \"Bad Name\" },",
            "idempotency_header",
        ),
        ("retry = { budget = {} },", "budget"),
        ("retry = { budget = { per_second = 0 } },", "budget"),
        ("retry = { budget = { ratio_percent = 101 } },", "budget"),
    ] {
        let message = error(&config(
            &proxy_route(
                "          policy = { request_buffering = true },",
                &format!("              {policy}"),
            ),
            endpoint,
        ));
        assert!(message.contains(expected), "{policy}: {message}");
    }
}

#[test]
fn validates_rescue_and_any_retry_status() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let rescue = proxy_route(
        "",
        r"              retry = { max_retries = 2, response_statuses = { 404, 418 }, triggers = {} },
              rescue = { statuses = { 404 } },",
    );
    let loaded = load_lua(&config(&rescue, endpoint)).expect("rescue policy");
    let policy = &first_route(&config(&rescue, endpoint))["action"]["policy"];
    assert_eq!(policy["retry"]["response_statuses"], json!([404, 418]));
    assert_eq!(
        policy["rescue"],
        json!({ "statuses": [404], "target": "next_server", "pool": null })
    );
    let rendered = render_lua(&loaded).expect("render rescue");
    assert_eq!(load_lua(&rendered).expect("rescue roundtrip"), loaded);

    for (policy, expected) in [
        (
            "retry = { max_retries = 1, response_statuses = { 99 } },",
            "response_statuses",
        ),
        (
            "retry = { max_retries = 1, response_statuses = { 600 } },",
            "response_statuses",
        ),
        ("rescue = { statuses = { 404 } },", "rescue"),
        (
            "retry = { max_retries = 1 }, rescue = { statuses = {} },",
            "sequence",
        ),
        (
            "retry = { max_retries = 1 }, rescue = { statuses = { 99 } },",
            "rescue",
        ),
        (
            "retry = { max_retries = 1 }, rescue = { statuses = { 404, 404 } },",
            "rescue",
        ),
        (
            "retry = { max_retries = 1 }, rescue = { statuses = { 404 }, target = \"pool\" },",
            "rescue",
        ),
        (
            "retry = { max_retries = 1 }, rescue = { statuses = { 404 }, pool = \"web\" },",
            "rescue",
        ),
        (
            "retry = { max_retries = 1 }, rescue = { statuses = { 404 }, target = \"pool\", pool = \"web\" },",
            "rescue",
        ),
        (
            "retry = { max_retries = 1 }, rescue = { statuses = { 404 }, target = \"pool\", pool = \"gone\" },",
            "gone",
        ),
    ] {
        let message = error(&config(
            &proxy_route("", &format!("              {policy}")),
            endpoint,
        ));
        assert!(message.contains(expected), "{policy}: {message}");
    }
}

#[test]
fn validates_explicit_retry_triggers_and_safety_rules() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    for policy in [
        r"              retry = { max_retries = 4 },",
        r"              retry = { max_retries = 1, triggers = {} },",
        r#"              retry = { triggers = { "connect_failure", "connect_failure" } },"#,
        r#"              retry = { triggers = { "response_status" } },"#,
        r#"              retry = { method_safety = "all" },"#,
        r#"              retry = { body_safety = "buffered" },"#,
        r"              retry = { max_retries = 1, final_redispatch = true },",
    ] {
        let route = proxy_route("", policy);
        let error = error(&config(&route, endpoint));
        assert!(
            error.contains("retry")
                || error.contains("unknown variant")
                || error.contains("expected a sequence"),
            "{error}"
        );
    }

    let mut default_config =
        load_lua(&config(&proxy_route("", ""), endpoint)).expect("default retry");
    {
        let HttpRouteAction::Proxy { policy, .. } =
            &mut default_config.http_services[0].routes[0].action
        else {
            panic!("proxy action");
        };
        policy.retry.triggers.clear();
        policy.retry.max_retries = 1;
    }
    assert!(matches!(
        default_config.clone().validate(),
        Err(ConfigError::InvalidHttpRoute {
            field: "action.policy.retry.triggers",
            ..
        })
    ));
    let HttpRouteAction::Proxy { policy, .. } =
        &mut default_config.http_services[0].routes[0].action
    else {
        panic!("proxy action");
    };
    policy.retry.max_retries = 0;
    default_config
        .validate()
        .expect("zero retries may disable all retry triggers");

    let status_only = proxy_route(
        "",
        r"              retry = {
                max_retries = 1,
                triggers = {},
                response_statuses = { 503 },
              },",
    );
    let status_only_config = load_lua(&config(&status_only, endpoint)).expect("status-only retry");
    let HttpRouteAction::Proxy { policy, .. } =
        &status_only_config.http_services[0].routes[0].action
    else {
        panic!("status-only proxy action");
    };
    assert!(policy.retry.triggers.is_empty());
    assert_eq!(policy.retry.response_statuses, [503]);
    let rendered = render_lua(&status_only_config).expect("render status-only retry");
    assert_eq!(
        load_lua(&rendered).expect("status-only retry roundtrip"),
        status_only_config
    );

    let route = proxy_route(
        "",
        r#"              retry = {
                max_retries = 3,
                target = "same_server",
                delay_ms = 1000,
                final_redispatch = true,
                triggers = { "connect_failure", "connect_timeout" },
              },"#,
    );
    let loaded = load_lua(&config(&route, endpoint)).expect("final redispatch policy");
    let rendered = render_lua(&loaded).expect("render final redispatch");
    assert!(rendered.contains("final_redispatch = true"));

    let route = proxy_route(
        "          policy = { request_buffering = true },",
        r#"              retry = {
                max_retries = 2,
                target = "next_server",
                triggers = { "response_timeout" },
                method_safety = "all",
                body_safety = "buffered",
              },"#,
    );
    let loaded = load_lua(&config(&route, endpoint)).expect("buffered idempotent retry policy");
    let rendered = render_lua(&loaded).expect("render buffered idempotent retry policy");
    assert!(rendered.contains("method_safety = \"all\""));
    assert!(rendered.contains("body_safety = \"buffered\""));
    assert_eq!(
        load_lua(&rendered).expect("buffered idempotent retry roundtrip"),
        loaded
    );
}

#[test]
fn rejects_the_removed_proxy_only_route_and_service_fields() {
    let old_route = r#"        {
          host = "example.com",
          path_prefix = "/",
          methods = { "GET" },
          upstream_pool = "web",
        },"#;
    let old_error = error(&config(
        old_route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ));
    assert!(!old_error.is_empty());

    for field in ["path_prefix", "upstream_pool"] {
        let route = proxy_route(&format!("          {field} = \"legacy\","), "");
        assert!(
            error(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains(&format!("unknown field `{field}`"))
        );
    }

    let source = config(
        &proxy_route("", ""),
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    )
    .replace(
        "      routes = {",
        "      max_retries = 1,\n      routes = {",
    );
    assert!(error(&source).contains("unknown field `max_retries`"));

    for field in ["buffering", "compression", "logging", "dynamic_origin"] {
        let route = proxy_route("", &format!("              {field} = true,"));
        assert!(
            error(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains(&format!("unknown field `{field}`"))
        );
    }
}

#[test]
fn retains_source_and_render_size_bounds_for_http_actions() {
    let oversized = "x".repeat(1024 * 1024);
    let route = format!(
        r#"        {{
          path = {{ kind = "exact", value = "/" }},
          action = {{ type = "fixed_response", status = 200, body = "{oversized}" }},
        }},"#
    );
    assert!(matches!(
        load_lua_error(&config(
            &route,
            r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        )),
        oxiroute_config_source::LuaConfigError::SourceTooLarge
    ));
}

#[test]
fn validates_header_pool_selectors() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let select = proxy_route(
        "",
        r#"              select = { header = "X-Min-VRAM", rules = { { at_most = 12, pool = "web" }, { at_most = 24, pool = "web" } }, on_invalid = "reject", spill = "up" },"#,
    );
    let loaded = load_lua(&config(&select, endpoint)).expect("selector");
    let policy = &first_route(&config(&select, endpoint))["action"]["policy"];
    assert_eq!(policy["select"]["header"], json!("x-min-vram"));
    assert_eq!(policy["select"]["forward"], json!(false));
    let rendered = render_lua(&loaded).expect("render selector");
    assert_eq!(load_lua(&rendered).expect("selector roundtrip"), loaded);

    for (select, expected) in [
        (r#"select = { header = "x", rules = {} },"#, "sequence"),
        (
            r#"select = { header = "x", rules = { { at_most = 24, pool = "web" }, { at_most = 12, pool = "web" } } },"#,
            "ascending",
        ),
        (
            r#"select = { header = "x", rules = { { at_most = 12, pool = "gone" } } },"#,
            "gone",
        ),
        (
            r#"select = { header = "host", rules = { { at_most = 12, pool = "web" } } },"#,
            "select",
        ),
        (
            r#"select = { header = "x", rules = { { at_most = 12, pool = "web" } }, spill = "down" },"#,
            "down",
        ),
    ] {
        let message = error(&config(
            &proxy_route("", &format!("              {select}")),
            endpoint,
        ));
        assert!(message.contains(expected), "{select}: {message}");
    }
}

#[test]
fn validates_sticky_session_affinity() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    for (affinity, field, value) in [
        (
            r#"affinity = { type = "cookie", name = "oxr_srv" },"#,
            "ttl_s",
            json!(3600),
        ),
        (
            r#"affinity = { type = "hash", key = "header:X-Session" },"#,
            "key",
            json!("header:x-session"),
        ),
        (
            r#"affinity = { type = "hash", key = "path_prefix:2" },"#,
            "key",
            json!("path_prefix:2"),
        ),
        (
            r#"affinity = { type = "header", name = "X-Worker" },"#,
            "name",
            json!("x-worker"),
        ),
    ] {
        let source = config(
            &proxy_route("", &format!("              {affinity}")),
            endpoint,
        );
        let loaded = load_lua(&source).expect(affinity);
        assert_eq!(
            first_route(&source)["action"]["policy"]["affinity"][field],
            value
        );
        let rendered = render_lua(&loaded).expect("render affinity");
        assert_eq!(load_lua(&rendered).expect("affinity roundtrip"), loaded);
    }

    for (affinity, expected) in [
        (
            r#"affinity = { type = "cookie", name = "bad name" },"#,
            "cookie name",
        ),
        (
            r#"affinity = { type = "cookie", name = "a", ttl_s = 0 },"#,
            "ttl_s",
        ),
        (
            r#"affinity = { type = "cookie", name = "a", same_site = "none", secure = false },"#,
            "secure",
        ),
        (
            r#"affinity = { type = "hash", key = "cookie:x" },"#,
            "key must be",
        ),
        (
            r#"affinity = { type = "hash", key = "path_prefix:9" },"#,
            "path_prefix",
        ),
        (
            r#"affinity = { type = "header", name = "host" },"#,
            "affinity",
        ),
    ] {
        let message = error(&config(
            &proxy_route("", &format!("              {affinity}")),
            endpoint,
        ));
        assert!(message.contains(expected), "{affinity}: {message}");
    }
}
