use oxiroute_config::{ConfigError, HttpRouteAction, load_lua, render_lua, validate_config};

fn config(routes: &str, endpoint: &str) -> String {
    format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{ name = "web", endpoints = {{ {endpoint} }} }},
  }},
  http_services = {{
    {{
      name = "web",
      routes = {{
{routes}
      }},
    }},
  }},
}}"#
    )
}

fn proxy_route(fields: &str, policy: &str) -> String {
    format!(
        r#"        {{
          path = {{ kind = "segment_prefix", value = "/" }},
{fields}
          action = {{
            type = "proxy",
            upstream_pool = "web",
            policy = {{
{policy}
            }},
          }},
        }},"#
    )
}

fn error(source: &str) -> String {
    load_lua(source)
        .expect_err("configuration must be rejected")
        .to_string()
}

fn first_route(source: &str) -> serde_json::Value {
    let config = load_lua(source).expect("HTTP route configuration");
    serde_json::to_value(config).expect("serialized configuration")["http_services"][0]["routes"][0]
        .clone()
}

#[test]
fn applies_the_explicit_safe_proxy_defaults() {
    let route = first_route(&config(
        &proxy_route("", ""),
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ));

    assert_eq!(route["host"], serde_json::Value::Null);
    assert_eq!(route["path"]["kind"], "segment_prefix");
    assert_eq!(route["path"]["value"], "/");
    assert_eq!(route["methods"], serde_json::json!([]));
    assert_eq!(route["access_policy"], serde_json::Value::Null);
    assert_eq!(route["action"]["type"], "proxy");
    assert_eq!(route["action"]["upstream_pool"], "web");
    let policy = &route["action"]["policy"];
    assert_eq!(policy["upstream_host"]["type"], "preserve_incoming");
    assert_eq!(policy["upstream_path_rewrite"], serde_json::Value::Null);
    assert_eq!(policy["request_headers"], serde_json::json!([]));
    assert_eq!(policy["response_headers"], serde_json::json!([]));
    assert_eq!(
        policy["response_cookie_path_rewrites"],
        serde_json::json!([])
    );
    assert_eq!(policy["retry"]["max_retries"], 0);
    assert_eq!(
        policy["retry"]["triggers"],
        serde_json::json!(["connect_failure", "connect_timeout", "refused_stream"])
    );
    assert_eq!(policy["retry"]["method_safety"], "get_head");
    assert_eq!(policy["retry"]["body_safety"], "empty");
    assert_eq!(policy["retry"]["final_redispatch"], false);
}

#[test]
fn rejects_unbounded_response_buffering() {
    let source = config(
        &proxy_route(
            "          policy = { max_request_body_bytes = null, response_buffering = true },",
            "",
        ),
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    );
    assert!(matches!(
        load_lua(&source).expect_err("unbounded response buffering"),
        ConfigError::InvalidHttpRoute {
            field: "policy.response_buffering",
            ..
        }
    ));
}

#[test]
fn applies_fixed_redirect_and_static_action_defaults() {
    let routes = r#"        {
          path = { kind = "exact", value = "/fixed" },
          action = { type = "fixed_response", status = 200 },
        },
        {
          path = { kind = "exact", value = "/redirect" },
          action = {
            type = "redirect",
            location = { kind = "literal", value = "/target" },
          },
        },
        {
          path = { kind = "segment_prefix", value = "/assets" },
          action = { type = "static_files", root_directory = "/srv/www" },
        },"#;
    let loaded = load_lua(&config(
        routes,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("action defaults");
    let value = serde_json::to_value(loaded).expect("serialized actions");
    let routes = &value["http_services"][0]["routes"];

    assert_eq!(routes[0]["action"]["body"], "");
    assert_eq!(routes[0]["action"]["headers"], serde_json::json!([]));
    assert_eq!(routes[1]["action"]["status"], 302);
    assert_eq!(
        routes[2]["action"]["index_files"],
        serde_json::json!(["index.html"])
    );
    assert_eq!(routes[2]["action"]["etag"], true);
    assert_eq!(routes[2]["action"]["spa_fallback"], serde_json::Value::Null);
}

#[test]
fn normalizes_host_and_path_selectors_without_collapsing_semantics() {
    let routes = [
        proxy_route(
            r#"          host = { kind = "normalized_host", value = "EXAMPLE.COM:8443" },
          path = { kind = "segment_prefix", value = "/api%3azone/" },
          methods = { "post", "GET" },"#,
            "",
        ),
        proxy_route(
            r#"          host = { kind = "exact_authority", value = "Example.COM:8443" },
          path = { kind = "raw_prefix", value = "/api" },"#,
            "",
        ),
        proxy_route(
            r#"          path = { kind = "exact", value = "/api" },"#,
            "",
        ),
        proxy_route(
            r#"          path = { kind = "ascii_case_insensitive_exact", value = "/Health" },"#,
            "",
        ),
    ]
    .join("\n");
    let loaded = load_lua(&config(
        &routes,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("selector variants");
    let value = serde_json::to_value(&loaded).expect("serialized selectors");
    let routes = value["http_services"][0]["routes"]
        .as_array()
        .expect("routes");

    assert_eq!(routes[0]["host"]["value"], "example.com");
    assert_eq!(routes[0]["path"]["value"], "/api%3Azone/");
    assert_eq!(routes[0]["methods"], serde_json::json!(["GET", "POST"]));
    assert_eq!(routes[1]["host"]["value"], "Example.COM:8443");
    assert_eq!(routes[1]["path"]["kind"], "raw_prefix");
    assert_eq!(routes[2]["path"]["kind"], "exact");
    assert_eq!(routes[3]["path"]["kind"], "ascii_case_insensitive_exact");

    let rendered = render_lua(&loaded).expect("rendered selectors");
    assert_eq!(
        render_lua(&load_lua(&rendered).expect("selector reload")).expect("second render"),
        rendered
    );
}

#[test]
fn ascii_case_insensitive_exact_paths_reject_non_ascii_values() {
    let route = proxy_route(
        r#"          path = { kind = "ascii_case_insensitive_exact", value = "/saude" },"#,
        "",
    )
    .replace("/saude", "/saúde");
    let error = error(&config(
        &route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ));

    assert!(error.contains("ASCII path"));
}

#[test]
fn validates_host_selector_kinds_and_authorities() {
    for value in ["*.EXAMPLE.COM:443", "127.0.0.1:8080", "[2001:0DB8::1]:443"] {
        let route = proxy_route(
            &format!("          host = {{ kind = \"normalized_host\", value = \"{value}\" }},"),
            "",
        );
        load_lua(&config(
            &route,
            r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        ))
        .unwrap_or_else(|error| panic!("valid normalized host {value}: {error}"));
    }

    for (kind, value) in [
        ("normalized_host", "user@example.com"),
        ("normalized_host", "example.com:not-a-port"),
        ("exact_authority", "*.example.com"),
        ("exact_authority", "user@example.com"),
        ("exact_authority", "example.com:not-a-port"),
    ] {
        let route = proxy_route(
            &format!("          host = {{ kind = \"{kind}\", value = \"{value}\" }},"),
            "",
        );
        assert!(
            error(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains("invalid `host`")
        );
    }
}

#[test]
fn canonicalizes_ascii_case_insensitive_exact_authority_without_dropping_ports() {
    let routes = proxy_route(
        r#"          host = { kind = "ascii_case_insensitive_exact_authority", value = "API.Example.COM:8443" },"#,
        "",
    );
    let loaded = load_lua(&config(
        &routes,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("case-insensitive exact authority");
    let route =
        serde_json::to_value(loaded).expect("serialized selector")["http_services"][0]["routes"][0]
            .clone();

    assert_eq!(
        route["host"]["kind"],
        "ascii_case_insensitive_exact_authority"
    );
    assert_eq!(route["host"]["value"], "api.example.com:8443");
}

#[test]
fn includes_selector_kind_in_duplicate_identity() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let kinds = ["segment_prefix", "raw_prefix", "exact"]
        .map(|kind| {
            proxy_route(
                &format!("          path = {{ kind = \"{kind}\", value = \"/api\" }},"),
                "",
            )
        })
        .join("\n");
    load_lua(&config(&kinds, endpoint)).expect("path selector kinds are distinct");

    let host_kinds = ["normalized_host", "exact_authority"]
        .map(|kind| {
            proxy_route(
                &format!("          host = {{ kind = \"{kind}\", value = \"example.com\" }},"),
                "",
            )
        })
        .join("\n");
    load_lua(&config(&host_kinds, endpoint)).expect("host selector kinds are distinct");

    let duplicate = proxy_route(
        r#"          path = { kind = "exact", value = "/api" },
          methods = { "POST", "GET" },"#,
        "",
    ) + &proxy_route(
        r#"          path = { kind = "exact", value = "/api" },
          methods = { "get", "post" },"#,
        "",
    );
    assert!(error(&config(&duplicate, endpoint)).contains("equivalent matchers"));
}

#[test]
fn bounds_and_canonicalizes_route_methods() {
    let sixteen = (0..16)
        .map(|index| format!("X{index}"))
        .collect::<Vec<_>>()
        .join("\", \"");
    let route = proxy_route(&format!("          methods = {{ \"{sixteen}\" }},"), "");
    load_lua(&config(
        &route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("16 methods");

    for methods in [
        (0..17).map(|index| format!("X{index}")).collect::<Vec<_>>(),
        vec!["x".repeat(33)],
        vec!["GE T".into()],
        vec!["GET".into(), "get".into()],
    ] {
        let methods = methods
            .iter()
            .map(|method| format!("\"{method}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let route = proxy_route(&format!("          methods = {{ {methods} }},"), "");
        assert!(
            error(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains("methods")
        );
    }
}

#[test]
fn renders_every_proxy_policy_variant_and_field() {
    let route = proxy_route(
        r#"          access_policy = {
            type = "bearer_token_file",
            token_file_path = "/run/secrets/api-token",
            header_name = "X-Api-Token",
            realm = "private-api",
          },"#,
        r#"              upstream_host = { type = "endpoint", unix_fallback = "fallback.internal:8080" },
              upstream_path_rewrite = { from = "/", to = "/application/" },
               request_headers = {
                { operation = "set", name = "X-Literal", value = { type = "literal", value = "edge" } },
                { operation = "set", name = "X-Authority", value = { type = "incoming_authority" } },
                { operation = "set", name = "X-Host", value = { type = "normalized_host" } },
                { operation = "set", name = "X-Client-Ip", value = { type = "client_ip" } },
                { operation = "set", name = "X-Upstream", value = { type = "selected_upstream_host" } },
                { operation = "remove", name = "X-Remove" },
              },
              response_headers = {
                { operation = "set", name = "X-Frame", value = "same-origin" },
                { operation = "remove", name = "X-Remove" },
              },
               response_cookie_path_rewrites = {
                 { from = "/", to = "/application" },
               },
               response_cookie_attributes = {
                 { name = "session", secure = true, http_only = false, same_site = "lax" },
               },
               retry = {
                max_retries = 2,
                triggers = { "connect_timeout", "refused_stream" },
                method_safety = "get_head",
                body_safety = "empty",
              },"#,
    );
    let loaded = load_lua(&config(
        &route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("complete proxy policy");
    let rendered = render_lua(&loaded).expect("rendered proxy policy");

    for field in [
        "access_policy",
        "token_file_path",
        "header_name",
        "realm",
        "action",
        "policy",
        "upstream_host",
        "upstream_path_rewrite",
        "unix_fallback",
        "request_headers",
        "response_headers",
        "response_cookie_path_rewrites",
        "response_cookie_attributes",
        "retry",
        "max_retries",
        "triggers",
        "method_safety",
        "body_safety",
    ] {
        assert!(rendered.contains(&format!("{field} =")), "missing {field}");
    }
    assert_eq!(load_lua(&rendered).expect("proxy reload"), loaded);
}

#[test]
fn validates_proxy_path_rewrite_as_two_canonical_absolute_paths() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let valid = proxy_route(
        "",
        r#"              upstream_path_rewrite = { from = "/api/", to = "/v1/" },"#,
    );
    load_lua(&config(&valid, endpoint)).expect("valid upstream path rewrite");

    for rewrite in [
        r#"{ from = "api/", to = "/v1/" }"#,
        r#"{ from = "/api/", to = "/v1/?query=1" }"#,
        r#"{ from = "/api/../", to = "/v1/" }"#,
    ] {
        let route = proxy_route(
            "",
            &format!("              upstream_path_rewrite = {rewrite},"),
        );
        assert!(error(&config(&route, endpoint)).contains("upstream_path_rewrite"));
    }
}

#[test]
fn rejects_invalid_or_conflicting_header_mutations() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    for policy in [
        r#"              request_headers = { { operation = "remove", name = "Connection" } },"#,
        r#"              request_headers = { { operation = "set", name = "Connection", value = { type = "literal", value = "close" } } },"#,
        r#"              request_headers = { { operation = "set", name = "Upgrade", value = { type = "literal", value = "websocket" } } },"#,
        r#"              request_headers = { { operation = "set", name = "Upgrade", value = { type = "incoming_header", name = "X-Upgrade", max_bytes = 128 } } },"#,
        r#"              request_headers = { { operation = "set", name = "Content-Length", value = { type = "literal", value = "1" } } },"#,
        r#"              request_headers = { { operation = "set", name = "Bad Name", value = { type = "literal", value = "x" } } },"#,
        r#"              request_headers = { { operation = "set", name = "X-Test", value = { type = "literal", value = "line\nvalue" } } },"#,
        r#"              response_headers = { { operation = "set", name = "Transfer-Encoding", value = "chunked" } },"#,
        r#"              response_headers = {
                { operation = "set", name = "X-Test", value = "one" },
                { operation = "remove", name = "x-test" },
              },"#,
    ] {
        let route = proxy_route("", policy);
        assert!(error(&config(&route, endpoint)).contains("header"));
    }
}

#[test]
fn validates_and_renders_x_forwarded_for_source_exceptions() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let policy = r#"              request_headers = {
                { operation = "set", name = "X-Forwarded-For", value = { type = "appended_x_forwarded_for", max_bytes = 8192, except_source_cidrs = { "127.0.0.0/8", "2001:db8::/32" } } },
              },"#;
    let loaded = load_lua(&config(&proxy_route("", policy), endpoint)).expect("valid XFF policy");
    let rendered = render_lua(&loaded).expect("render XFF policy");
    assert!(rendered.contains("except_source_cidrs"));
    assert!(rendered.contains("127.0.0.0/8"));
    assert_eq!(load_lua(&rendered).expect("reload XFF policy"), loaded);

    for invalid in [
        r#"{ operation = "set", name = "X-Other", value = { type = "appended_x_forwarded_for", max_bytes = 8192 } }"#,
        r#"{ operation = "set", name = "X-Forwarded-For", value = { type = "appended_x_forwarded_for", max_bytes = 8192, except_source_cidrs = { "127.0.0.1/8" } } }"#,
        r#"{ operation = "set", name = "X-Forwarded-For", value = { type = "appended_x_forwarded_for", max_bytes = 8192, except_source_cidrs = { "127.0.0.0/8", "127.0.0.0/8" } } }"#,
    ] {
        let policy = format!("              request_headers = {{ {invalid} }},");
        assert!(error(&config(&proxy_route("", &policy), endpoint)).contains("header"));
    }
}

#[test]
fn accepts_only_the_pingora_managed_websocket_header_idiom() {
    let route = proxy_route(
        "",
        r#"              request_headers = {
                { operation = "set", name = "Upgrade", value = { type = "incoming_header", name = "Upgrade", max_bytes = 128 } },
                { operation = "set", name = "Connection", value = { type = "literal", value = "upgrade" } },
              },"#,
    );

    load_lua(&config(
        &route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("standard nginx WebSocket headers are managed by Pingora");
}

#[test]
fn enforces_header_and_cookie_policy_bounds() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let request_headers = (0..32)
        .map(|index| format!(r#"{{ operation = "remove", name = "X-Request-{index}" }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let response_headers = (0..32)
        .map(|index| format!(r#"{{ operation = "remove", name = "X-Response-{index}" }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let rewrites = (0..16)
        .map(|index| format!(r#"{{ from = "/{index}", to = "/target/{index}" }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let cookie_attributes = (0..16)
        .map(|index| format!(r#"{{ name = "cookie-{index}", secure = true }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let policy = format!(
        "              request_headers = {{ {request_headers} }},\n              response_headers = {{ {response_headers} }},\n              response_cookie_path_rewrites = {{ {rewrites} }},\n              response_cookie_attributes = {{ {cookie_attributes} }},"
    );
    load_lua(&config(&proxy_route("", &policy), endpoint)).expect("collection boundaries");

    for policy in [
        format!(
            "              request_headers = {{ {request_headers}, {{ operation = \"remove\", name = \"X-Request-32\" }} }},"
        ),
        format!(
            "              response_headers = {{ {response_headers}, {{ operation = \"remove\", name = \"X-Response-32\" }} }},"
        ),
        format!(
            "              response_cookie_path_rewrites = {{ {rewrites}, {{ from = \"/16\", to = \"/target/16\" }} }},"
        ),
        format!(
            "              response_cookie_attributes = {{ {cookie_attributes}, {{ name = \"cookie-16\", secure = true }} }},"
        ),
    ] {
        assert!(error(&config(&proxy_route("", &policy), endpoint)).contains("at most"));
    }
}

#[test]
fn enforces_literal_value_and_realm_byte_bounds() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let header_boundary = "a".repeat(8 * 1024);
    let route = proxy_route(
        "",
        &format!(
            r#"              request_headers = {{ {{ operation = "set", name = "X-Test", value = {{ type = "literal", value = "{header_boundary}" }} }} }},"#
        ),
    );
    load_lua(&config(&route, endpoint)).expect("8192-byte header value");
    let too_long = format!("{header_boundary}a");
    let route = proxy_route(
        "",
        &format!(
            r#"              request_headers = {{ {{ operation = "set", name = "X-Test", value = {{ type = "literal", value = "{too_long}" }} }} }},"#
        ),
    );
    assert!(error(&config(&route, endpoint)).contains("8192 bytes"));

    let realm_boundary = "a".repeat(128);
    let route = proxy_route(
        &format!(
            r#"          access_policy = {{ type = "bearer_token_file", token_file_path = "/run/token", realm = "{realm_boundary}" }},"#
        ),
        "",
    );
    load_lua(&config(&route, endpoint)).expect("128-byte realm");
    let too_long = format!("{realm_boundary}a");
    let route = proxy_route(
        &format!(
            r#"          access_policy = {{ type = "bearer_token_file", token_file_path = "/run/token", realm = "{too_long}" }},"#
        ),
        "",
    );
    assert!(error(&config(&route, endpoint)).contains("128"));
}

#[test]
fn enforces_action_payload_and_static_index_bounds() {
    let endpoint = r#"{ type = "socket", address = "127.0.0.1:3000" }"#;
    let body_boundary = "x".repeat(64 * 1024);
    let route = format!(
        r#"        {{ path = {{ kind = "exact", value = "/fixed" }}, action = {{ type = "fixed_response", status = 200, body = "{body_boundary}" }} }},"#
    );
    load_lua(&config(&route, endpoint)).expect("65536-byte fixed body");
    let too_long = format!("{body_boundary}x");
    let route = format!(
        r#"        {{ path = {{ kind = "exact", value = "/fixed" }}, action = {{ type = "fixed_response", status = 200, body = "{too_long}" }} }},"#
    );
    assert!(error(&config(&route, endpoint)).contains("65536"));

    let location_boundary = format!("/{}", "a".repeat(2047));
    let route = format!(
        r#"        {{ path = {{ kind = "exact", value = "/redirect" }}, action = {{ type = "redirect", location = {{ kind = "literal", value = "{location_boundary}" }} }} }},"#
    );
    load_lua(&config(&route, endpoint)).expect("2048-byte redirect location");
    let too_long = format!("{location_boundary}a");
    let route = format!(
        r#"        {{ path = {{ kind = "exact", value = "/redirect" }}, action = {{ type = "redirect", location = {{ kind = "literal", value = "{too_long}" }} }} }},"#
    );
    assert!(error(&config(&route, endpoint)).contains("2048"));

    let indexes = (0..8)
        .map(|index| format!(r#""index-{index}.html""#))
        .collect::<Vec<_>>()
        .join(", ");
    let route = format!(
        r#"        {{ path = {{ kind = "segment_prefix", value = "/" }}, action = {{ type = "static_files", root_directory = "/srv/www", index_files = {{ {indexes} }} }} }},"#
    );
    load_lua(&config(&route, endpoint)).expect("eight index files");
    let route = route.replace(
        &format!("index_files = {{ {indexes} }}"),
        &format!("index_files = {{ {indexes}, \"index-8.html\" }}"),
    );
    assert!(error(&config(&route, endpoint)).contains("at most 8"));
}

#[test]
fn requires_a_literal_fallback_for_endpoint_host_on_unix_pools() {
    let unix = r#"{ type = "unix", path = "/run/oxiroute/backend.sock" }"#;
    let route = proxy_route(
        "",
        r#"              upstream_host = { type = "endpoint" },"#,
    );
    let error = error(&config(&route, unix));
    assert!(error.contains("Unix endpoint"));
    assert!(error.contains("unix_fallback"));

    let route = proxy_route(
        "",
        r#"              upstream_host = { type = "endpoint", unix_fallback = "backend.internal" },"#,
    );
    load_lua(&config(&route, unix)).expect("Unix endpoint fallback");
}

#[test]
fn validates_fixed_response_actions_and_literal_headers() {
    for status in [200, 204, 205, 304, 599] {
        let body = if matches!(status, 204 | 205 | 304) {
            ""
        } else {
            "hello"
        };
        let route = format!(
            r#"        {{
          path = {{ kind = "exact", value = "/health" }},
          action = {{
            type = "fixed_response",
            status = {status},
            body = "{body}",
            headers = {{ {{ name = "X-Source", value = "oxiroute" }} }},
          }},
        }},"#
        );
        load_lua(&config(
            &route,
            r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        ))
        .unwrap_or_else(|error| panic!("valid status {status}: {error}"));
    }

    for (status, body) in [
        (199, ""),
        (600, ""),
        (204, "body"),
        (205, "body"),
        (304, "body"),
    ] {
        let route = format!(
            r#"        {{
          path = {{ kind = "exact", value = "/health" }},
          action = {{ type = "fixed_response", status = {status}, body = "{body}" }},
        }},"#
        );
        assert!(
            error(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains("fixed_response")
        );
    }
}

#[test]
fn validates_redirect_statuses_and_the_closed_template_language() {
    for (kind, value) in [
        ("literal", "https://example.com/target"),
        ("request_template", "$scheme://$host$request_uri"),
    ] {
        for status in [301, 302, 307, 308] {
            let route = format!(
                r#"        {{
          path = {{ kind = "segment_prefix", value = "/" }},
          action = {{
            type = "redirect",
            status = {status},
            location = {{ kind = "{kind}", value = "{value}" }},
          }},
        }},"#
            );
            let loaded = load_lua(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .expect("valid redirect");
            let rendered = render_lua(&loaded).expect("rendered redirect");
            assert_eq!(load_lua(&rendered).expect("redirect reload"), loaded);
        }
    }

    for value in ["$uri", "$request_method", "$", "$host\\nInjected"] {
        let route = format!(
            r#"        {{
          path = {{ kind = "segment_prefix", value = "/" }},
          action = {{
            type = "redirect",
            location = {{ kind = "request_template", value = "{value}" }},
          }},
        }},"#
        );
        assert!(
            error(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains("location")
        );
    }
}

#[test]
fn validates_static_file_roots_indexes_and_spa_fallbacks_lexically() {
    let route = r#"        {
          path = { kind = "segment_prefix", value = "/assets" },
          action = {
            type = "static_files",
            root_directory = "/srv//www///application",
            index_files = { "index.html", "home.htm" },
            spa_fallback = "application/index.html",
          },
        },"#;
    let loaded = load_lua(&config(
        route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("static file action");
    let value = serde_json::to_value(loaded).expect("serialized static route");
    assert_eq!(
        value["http_services"][0]["routes"][0]["action"]["root_directory"],
        "/srv/www/application"
    );

    for (field, value) in [
        ("root_directory", "srv/www"),
        ("root_directory", "/srv/../www"),
        ("index_files", "../index.html"),
        ("index_files", ".hidden"),
        ("spa_fallback", "../index.html"),
        ("spa_fallback", "/index.html"),
        ("spa_fallback", "app//index.html"),
    ] {
        let action_fields = if field == "index_files" {
            format!(
                "            root_directory = \"/srv/www\",\n            index_files = {{ \"{value}\" }},"
            )
        } else if field == "spa_fallback" {
            format!(
                "            root_directory = \"/srv/www\",\n            spa_fallback = \"{value}\","
            )
        } else {
            format!("            root_directory = \"{value}\",")
        };
        let route = format!(
            r#"        {{
          path = {{ kind = "segment_prefix", value = "/" }},
          action = {{
            type = "static_files",
{action_fields}
          }},
        }},"#
        );
        assert!(
            error(&config(
                &route,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains(field)
        );
    }

    let route = r#"        {
      path = { kind = "segment_prefix", value = "/" },
      action = {
        type = "static_files",
        root_directory = "/srv/www",
        content_type = "text/plain",
      },
    },"#;
    assert!(
        error(&config(
            route,
            r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        ))
        .contains("unknown field `content_type`")
    );
}

#[test]
fn static_etag_policy_renders_exact_booleans_and_rejects_invalid_forms() {
    let route = r#"        {
      path = { kind = "segment_prefix", value = "/assets" },
      action = {
        type = "static_files",
        root_directory = "/srv/www",
        etag = false,
      },
    },"#;
    let loaded = load_lua(&config(
        route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ))
    .expect("disabled static ETag policy");
    let rendered = render_lua(&loaded).expect("render static ETag policy");
    assert!(rendered.contains("etag = false"));
    assert_eq!(
        load_lua(&rendered).expect("reload static ETag policy"),
        loaded
    );

    for value in [r#""off""#, "0", "{}"] {
        let invalid = route.replace("false", value);
        assert!(
            error(&config(
                &invalid,
                r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
            ))
            .contains("etag"),
            "accepted static etag = {value}"
        );
    }
}

#[test]
fn validates_secret_backed_bearer_access_without_inline_tokens() {
    let route = proxy_route(
        r#"          access_policy = {
            type = "bearer_token_file",
            token_file_path = "/run/secrets/api-token",
          },"#,
        "",
    );
    let value = first_route(&config(
        &route,
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
    ));
    assert_eq!(value["access_policy"]["header_name"], "authorization");
    assert_eq!(value["access_policy"]["realm"], serde_json::Value::Null);

    for policy in [
        r#"{ type = "bearer_token_file", token_file_path = "run/token" }"#,
        r#"{ type = "bearer_token_file", token_file_path = "/run/token", header_name = "Bad Name" }"#,
        r#"{ type = "bearer_token_file", token_file_path = "/run/token", realm = "bad\nrealm" }"#,
        r#"{ type = "bearer_token_file", token_file_path = "/run/token", token = "inline" }"#,
    ] {
        let route = proxy_route(&format!("          access_policy = {policy},"), "");
        let error = error(&config(
            &route,
            r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        ));
        assert!(error.contains("access") || error.contains("unknown field"));
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
    let HttpRouteAction::Proxy { policy, .. } =
        &mut default_config.http_services[0].routes[0].action
    else {
        panic!("proxy action");
    };
    policy.retry.triggers.clear();
    assert!(matches!(
        validate_config(&mut default_config),
        Err(ConfigError::InvalidHttpRoute {
            field: "action.policy.retry.triggers",
            ..
        })
    ));

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
        load_lua(&config(
            &route,
            r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        )),
        Err(ConfigError::SourceTooLarge)
    ));
}
