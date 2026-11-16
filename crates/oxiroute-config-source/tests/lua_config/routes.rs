use oxiroute_config::ConfigError;

use crate::common::{changed, error_from};
use crate::lua_support::load_lua;

const WEB_ROUTES: &str = r#"      routes = {
        {
          host = { kind = "normalized_host", value = "example.com" },
          path = { kind = "segment_prefix", value = "/api" },
          methods = { "GET", "POST" },
          action = {
            type = "proxy",
            upstream_pool = "web-backends",
            policy = {},
          },
        },
      },"#;

#[test]
fn normalizes_exact_wildcard_and_ip_hosts() {
    let source = changed(
        WEB_ROUTES,
        r#"      routes = {
        {
          host = { kind = "normalized_host", value = "EXAMPLE.COM" },
          path = { kind = "segment_prefix", value = "/" },
          action = { type = "proxy", upstream_pool = "web-backends", policy = {} },
        },
        {
          host = { kind = "normalized_host", value = "*.API.EXAMPLE.COM" },
          path = { kind = "segment_prefix", value = "/" },
          action = { type = "proxy", upstream_pool = "web-backends", policy = {} },
        },
        {
          host = { kind = "normalized_host", value = "2001:0DB8:0:0:0:0:0:1" },
          path = { kind = "segment_prefix", value = "/" },
          action = { type = "proxy", upstream_pool = "web-backends", policy = {} },
        },
      },"#,
    );
    let config = load_lua(&source).expect("valid host matchers");
    let routes = &config.http_services[0].routes;

    let routes = serde_json::to_value(routes).expect("serialized routes");
    assert_eq!(routes[0]["host"]["value"], "example.com");
    assert_eq!(routes[1]["host"]["value"], "*.api.example.com");
    assert_eq!(routes[2]["host"]["value"], "2001:db8::1");
}

#[test]
fn preserves_path_selector_semantics_before_duplicate_detection() {
    let source = changed(
        "          path = { kind = \"segment_prefix\", value = \"/api\" },",
        "          path = { kind = \"segment_prefix\", value = \"/api/\" },",
    );
    let config = load_lua(&source).expect("normalized path prefix");

    assert_eq!(
        serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["path"]
            ["value"],
        "/api/"
    );

    let duplicate = changed(
        "        },\n      },\n      upstream_io_timeout_ms",
        r#"        },
        {
          host = { kind = "normalized_host", value = "example.com" },
          path = { kind = "segment_prefix", value = "/api/" },
          methods = { "POST", "GET" },
          action = {
            type = "proxy",
            upstream_pool = "database-backends",
            policy = {},
          },
        },
      },
      upstream_io_timeout_ms"#,
    );
    load_lua(&duplicate).expect("trailing slash retains distinct segment-prefix semantics");
}

#[test]
fn canonicalizes_percent_triplet_case_in_route_prefixes() {
    let source = changed(
        "          path = { kind = \"segment_prefix\", value = \"/api\" },",
        "          path = { kind = \"segment_prefix\", value = \"/api%3azone\" },",
    );
    let config = load_lua(&source).expect("canonical percent triplet");

    assert_eq!(
        serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["path"]
            ["value"],
        "/api%3Azone"
    );
}

#[test]
fn rejects_empty_http_routes() {
    let source = changed(WEB_ROUTES, "      routes = {},");
    let error = error_from(&source);

    assert!(matches!(error, ConfigError::EmptyHttpRoutes { service } if service == "web"));
}

#[test]
fn rejects_invalid_route_hosts() {
    for host in [
        "",
        "*.127.0.0.1",
        "api.*.example.com",
        "-api.example.com",
        "api..example.com",
    ] {
        let error = error_from(&changed(
            "          host = { kind = \"normalized_host\", value = \"example.com\" },",
            &format!("          host = {{ kind = \"normalized_host\", value = \"{host}\" }},"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidHttpRoute {
                service,
                route: 0,
                field: "host",
                ..
            } if service == "web"
        ));
    }
}

#[test]
fn rejects_invalid_route_path_prefixes() {
    for path_prefix in [
        "api",
        "/api?query",
        "/api#fragment",
        "/api path",
        "/api<internal",
        "/api>internal",
        "/api`internal",
        "/api/../internal",
        "/api//internal",
        "/api%2finternal",
        "/%61pi",
    ] {
        let error = error_from(&changed(
            "          path = { kind = \"segment_prefix\", value = \"/api\" },",
            &format!(
                "          path = {{ kind = \"segment_prefix\", value = \"{path_prefix}\" }},"
            ),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidHttpRoute { service, route: 0, field: "path", .. }
                if service == "web"
        ));
    }
}

#[test]
fn rejects_invalid_and_duplicate_route_methods() {
    for method in ["GE T", "G\u{c9}T", ""] {
        let error = error_from(&changed(
            "          methods = { \"GET\", \"POST\" },",
            &format!("          methods = {{ \"{method}\" }},"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidHttpRoute {
                service,
                route: 0,
                field: "methods",
                ..
            } if service == "web"
        ));
    }

    let error = error_from(&changed(
        "          methods = { \"GET\", \"POST\" },",
        "          methods = { \"GET\", \"get\" },",
    ));
    assert!(matches!(
        error,
        ConfigError::InvalidHttpRoute {
            service,
            route: 0,
            field: "methods",
            ..
        } if service == "web"
    ));
}

#[test]
fn rejects_duplicate_equivalent_routes_after_normalization() {
    let source = changed(
        "        },\n      },\n      upstream_io_timeout_ms",
        r#"        },
        {
          host = { kind = "normalized_host", value = "EXAMPLE.COM" },
          path = { kind = "segment_prefix", value = "/api" },
          methods = { "POST", "GET" },
          action = { type = "fixed_response", status = 200 },
        },
      },
      upstream_io_timeout_ms"#,
    );
    let error = error_from(&source);

    assert!(matches!(
        error,
        ConfigError::DuplicateHttpRoute {
            service,
            first_route: 0,
            duplicate_route: 1
        } if service == "web"
    ));
}

#[test]
fn rejects_unknown_route_and_l4_upstream_pools() {
    let error = error_from(&changed(
        "          upstream_pool = \"web-backends\",",
        "          upstream_pool = \"missing\",",
    ));
    assert!(matches!(
        error,
        ConfigError::UnknownRouteUpstreamPool {
            service,
            route: 0,
            pool
        } if service == "web" && pool == "missing"
    ));

    let error = error_from(&changed(
        "      upstream_pool = \"database-backends\",",
        "      upstream_pool = \"missing\",",
    ));
    assert!(matches!(
        error,
        ConfigError::UnknownL4UpstreamPool { service, pool }
            if service == "database" && pool == "missing"
    ));
}
