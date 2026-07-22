use oxiroute_config::{ConfigError, HealthCheckType, Protocol, UpstreamAlgorithm, load_lua};

const VALID_CONFIG: &str = r#"
return {
  version = 1,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  listeners = {
    {
      name = "web",
      bind = "127.0.0.1:8080",
      protocol = "http",
      service = "web",
      max_connections = 5000,
    },
    {
      name = "database",
      bind = "127.0.0.1:15432",
      protocol = "tcp",
      service = "database",
      max_connections = 1000,
    },
    {
      name = "live",
      bind = "127.0.0.1:1935",
      protocol = "rtmp",
      max_connections = 500,
    },
  },
  upstream_pools = {
    {
      name = "web-backends",
      endpoints = { "127.0.0.1:3000", "127.0.0.1:3001" },
      algorithm = "round_robin",
    },
    {
      name = "database-backends",
      endpoints = { "10.0.0.12:5432" },
      algorithm = "round_robin",
    },
  },
  http_services = {
    {
      name = "web",
      routes = {
        {
          host = "example.com",
          path_prefix = "/api",
          methods = { "GET", "POST" },
          upstream_pool = "web-backends",
        },
      },
      upstream_io_timeout_ms = 15000,
      max_request_body_bytes = 2097152,
    },
  },
  l4_services = {
    {
      name = "database",
      upstream_pool = "database-backends",
      connect_timeout_ms = 5000,
      idle_timeout_ms = 120000,
      lifetime_timeout_ms = 600000,
    },
  },
}
"#;

fn changed(from: &str, to: &str) -> String {
    assert_eq!(
        VALID_CONFIG.matches(from).count(),
        1,
        "fixture fragment must occur exactly once: {from}"
    );
    VALID_CONFIG.replacen(from, to, 1)
}

fn error_from(source: &str) -> ConfigError {
    load_lua(source).expect_err("configuration must be rejected")
}

#[test]
fn loads_the_canonical_configuration() {
    let config = load_lua(VALID_CONFIG).expect("valid canonical configuration");

    assert_eq!(config.version, 1);
    assert_eq!(config.management.expect("management").bind.port(), 9080);
    assert_eq!(config.listeners.len(), 3);
    assert_eq!(config.listeners[0].protocol, Protocol::Http);
    assert_eq!(config.listeners[0].service.as_deref(), Some("web"));
    assert_eq!(config.listeners[1].protocol, Protocol::Tcp);
    assert_eq!(config.listeners[2].protocol, Protocol::Rtmp);
    assert_eq!(config.listeners[2].service, None);
    assert_eq!(config.upstream_pools.len(), 2);
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::RoundRobin
    );
    assert_eq!(
        config.http_services[0].routes[0].host.as_deref(),
        Some("example.com")
    );
    assert_eq!(config.l4_services[0].lifetime_timeout_ms, Some(600_000));
}

#[test]
fn loads_the_distributed_example_configuration() {
    let config = load_lua(include_str!("../../../oxiroute.example.lua"))
        .expect("distributed example must remain valid");

    assert_eq!(config.listeners.len(), 3);
    assert_eq!(config.upstream_pools.len(), 2);
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(config.l4_services.len(), 1);
}

#[test]
fn applies_all_collection_and_field_defaults() {
    let minimal = load_lua(
        r#"
return {
  version = 1,
  listeners = {
    { name = "live", bind = "127.0.0.1:1935", protocol = "rtmp" },
  },
}
"#,
    )
    .expect("minimal configuration");

    assert_eq!(minimal.management, None);
    assert_eq!(minimal.listeners[0].max_connections, 10_000);
    assert!(minimal.upstream_pools.is_empty());
    assert!(minimal.http_services.is_empty());
    assert!(minimal.l4_services.is_empty());

    let source = VALID_CONFIG
        .replace("      max_connections = 5000,\n", "")
        .replace("      max_connections = 1000,\n", "")
        .replace("      max_connections = 500,\n", "")
        .replace("      algorithm = \"round_robin\",\n", "")
        .replace("          host = \"example.com\",\n", "")
        .replace("          path_prefix = \"/api\",\n", "")
        .replace("          methods = { \"GET\", \"POST\" },\n", "")
        .replace("      upstream_io_timeout_ms = 15000,\n", "")
        .replace("      max_request_body_bytes = 2097152,\n", "")
        .replace("      connect_timeout_ms = 5000,\n", "")
        .replace("      idle_timeout_ms = 120000,\n", "")
        .replace("      lifetime_timeout_ms = 600000,\n", "");
    let config = load_lua(&source).expect("configuration using field defaults");
    let route = &config.http_services[0].routes[0];

    assert!(
        config
            .listeners
            .iter()
            .all(|listener| listener.max_connections == 10_000)
    );
    assert!(
        config
            .upstream_pools
            .iter()
            .all(|pool| pool.algorithm == UpstreamAlgorithm::RoundRobin)
    );
    assert_eq!(route.host, None);
    assert_eq!(route.path_prefix, "/");
    assert!(route.methods.is_empty());
    assert_eq!(config.http_services[0].upstream_io_timeout_ms, 30_000);
    assert_eq!(config.http_services[0].max_retries, 0);
    assert_eq!(
        config.http_services[0].max_request_body_bytes,
        10 * 1024 * 1024
    );
    assert_eq!(config.l4_services[0].connect_timeout_ms, 10_000);
    assert_eq!(config.l4_services[0].idle_timeout_ms, 300_000);
    assert_eq!(config.l4_services[0].lifetime_timeout_ms, None);
}

#[test]
fn loads_a_bounded_http_retry_budget() {
    for max_retries in [1, 2] {
        let source = changed(
            "      max_request_body_bytes = 2097152,",
            &format!("      max_request_body_bytes = 2097152,\n      max_retries = {max_retries},"),
        );
        let config = load_lua(&source).expect("bounded retry budget");

        assert_eq!(config.http_services[0].max_retries, max_retries);
    }
}

#[test]
fn rejects_an_excessive_http_retry_budget() {
    let source = changed(
        "      max_request_body_bytes = 2097152,",
        "      max_request_body_bytes = 2097152,\n      max_retries = 3,",
    );
    let error = error_from(&source);

    assert!(matches!(
        error,
        ConfigError::RetryLimitTooLarge { service, limit: 3 } if service == "web"
    ));
}

#[test]
fn loads_tcp_and_http_health_check_policies() {
    let tcp_source = changed(
        "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"round_robin\",",
        "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"round_robin\",\n      health_check = { type = \"tcp\" },",
    );
    let tcp = load_lua(&tcp_source).expect("TCP health check");
    let tcp_check = tcp.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("TCP policy");
    assert_eq!(tcp_check.kind, HealthCheckType::Tcp);
    assert_eq!(tcp_check.interval_ms, 10_000);
    assert_eq!(tcp_check.timeout_ms, 1_000);
    assert_eq!(tcp_check.healthy_threshold, 1);
    assert_eq!(tcp_check.unhealthy_threshold, 3);

    let http_source = changed(
        "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"round_robin\",",
        r#"      endpoints = { "127.0.0.1:3000", "127.0.0.1:3001" },
      algorithm = "round_robin",
      health_check = {
        type = "http",
        interval_ms = 5000,
        timeout_ms = 500,
        healthy_threshold = 2,
        unhealthy_threshold = 4,
        host = "backend.internal:3000",
        path = "/healthz",
      },"#,
    );
    let http = load_lua(&http_source).expect("HTTP health check");
    let http_check = http.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("HTTP policy");
    assert_eq!(http_check.kind, HealthCheckType::Http);
    assert_eq!(http_check.interval_ms, 5_000);
    assert_eq!(http_check.timeout_ms, 500);
    assert_eq!(http_check.healthy_threshold, 2);
    assert_eq!(http_check.unhealthy_threshold, 4);
    assert_eq!(http_check.host.as_deref(), Some("backend.internal:3000"));
    assert_eq!(http_check.path.as_deref(), Some("/healthz"));
}

#[test]
fn rejects_invalid_health_check_timing_and_thresholds() {
    for policy in [
        r#"{ type = "tcp", interval_ms = 999 }"#,
        r#"{ type = "tcp", interval_ms = 86400001 }"#,
        r#"{ type = "tcp", interval_ms = 1000, timeout_ms = 1000 }"#,
        r#"{ type = "tcp", interval_ms = 40000, timeout_ms = 30001 }"#,
        r#"{ type = "tcp", healthy_threshold = 0 }"#,
        r#"{ type = "tcp", healthy_threshold = 101 }"#,
        r#"{ type = "tcp", unhealthy_threshold = 0 }"#,
        r#"{ type = "tcp", unhealthy_threshold = 101 }"#,
    ] {
        let source = changed(
            "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"round_robin\",",
            &format!(
                "      endpoints = {{ \"127.0.0.1:3000\", \"127.0.0.1:3001\" }},\n      algorithm = \"round_robin\",\n      health_check = {policy},"
            ),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }
}

#[test]
fn rejects_health_check_fields_that_do_not_match_the_probe_type() {
    for policy in [
        r#"{ type = "http", path = "/healthz" }"#,
        r#"{ type = "http", host = "backend.internal" }"#,
        r#"{ type = "http", host = "user@backend.internal", path = "/healthz" }"#,
        r#"{ type = "http", host = "backend.internal:not-a-port", path = "/healthz" }"#,
        r#"{ type = "http", host = "backend.internal", path = "healthz" }"#,
        r#"{ type = "http", host = "backend.internal", path = "/healthz?full=true" }"#,
        r#"{ type = "tcp", host = "backend.internal" }"#,
        r#"{ type = "tcp", path = "/healthz" }"#,
    ] {
        let source = changed(
            "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"round_robin\",",
            &format!(
                "      endpoints = {{ \"127.0.0.1:3000\", \"127.0.0.1:3001\" }},\n      algorithm = \"round_robin\",\n      health_check = {policy},"
            ),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }

    for policy in [
        format!(
            r#"{{ type = "http", host = "{}", path = "/healthz" }}"#,
            "a".repeat(256)
        ),
        format!(
            r#"{{ type = "http", host = "backend.internal", path = "/{}" }}"#,
            "a".repeat(2_048)
        ),
    ] {
        let source = changed(
            "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"round_robin\",",
            &format!(
                "      endpoints = {{ \"127.0.0.1:3000\", \"127.0.0.1:3001\" }},\n      algorithm = \"round_robin\",\n      health_check = {policy},"
            ),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }
}

#[test]
fn normalizes_exact_wildcard_and_ip_hosts() {
    let source = changed(
        r#"      routes = {
        {
          host = "example.com",
          path_prefix = "/api",
          methods = { "GET", "POST" },
          upstream_pool = "web-backends",
        },
      },"#,
        r#"      routes = {
        { host = "EXAMPLE.COM", upstream_pool = "web-backends" },
        { host = "*.API.EXAMPLE.COM", upstream_pool = "web-backends" },
        { host = "2001:0DB8:0:0:0:0:0:1", upstream_pool = "web-backends" },
      },"#,
    );
    let config = load_lua(&source).expect("valid host matchers");
    let routes = &config.http_services[0].routes;

    assert_eq!(routes[0].host.as_deref(), Some("example.com"));
    assert_eq!(routes[1].host.as_deref(), Some("*.api.example.com"));
    assert_eq!(routes[2].host.as_deref(), Some("2001:db8::1"));
}

#[test]
fn normalizes_route_path_prefixes_before_duplicate_detection() {
    let source = changed(
        "          path_prefix = \"/api\",",
        "          path_prefix = \"/api///\",",
    );
    let config = load_lua(&source).expect("normalized path prefix");

    assert_eq!(config.http_services[0].routes[0].path_prefix, "/api");

    let duplicate = changed(
        "          upstream_pool = \"web-backends\",\n        },",
        r#"          upstream_pool = "web-backends",
        },
        {
          host = "example.com",
          path_prefix = "/api/",
          methods = { "POST", "GET" },
          upstream_pool = "database-backends",
        },"#,
    );
    assert!(matches!(
        error_from(&duplicate),
        ConfigError::DuplicateHttpRoute { .. }
    ));
}

#[test]
fn canonicalizes_percent_triplet_case_in_route_prefixes() {
    let source = changed(
        "          path_prefix = \"/api\",",
        "          path_prefix = \"/api%3azone\",",
    );
    let config = load_lua(&source).expect("canonical percent triplet");

    assert_eq!(config.http_services[0].routes[0].path_prefix, "/api%3Azone");
}

#[test]
fn rejects_unsupported_versions() {
    let error = error_from(&changed("  version = 1,", "  version = 2,"));

    assert!(matches!(error, ConfigError::UnsupportedVersion(2)));
}

#[test]
fn rejects_non_loopback_and_zero_port_management_binds() {
    let error = error_from(&changed("127.0.0.1:9080", "0.0.0.0:9080"));
    assert!(matches!(error, ConfigError::ManagementMustUseLoopback(_)));

    let error = error_from(&changed("127.0.0.1:9080", "127.0.0.1:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "management listener",
            name,
            field: "bind"
        } if name == "management"
    ));
}

#[test]
fn rejects_blank_names_in_every_namespace() {
    let cases = [
        (
            "      name = \"web\",\n      bind",
            "      name = \"  \",\n      bind",
            "listener",
        ),
        (
            "      name = \"web-backends\",",
            "      name = \"  \",",
            "upstream pool",
        ),
        (
            "      name = \"web\",\n      routes",
            "      name = \"  \",\n      routes",
            "HTTP service",
        ),
        (
            "      name = \"database\",\n      upstream_pool",
            "      name = \"  \",\n      upstream_pool",
            "L4 service",
        ),
    ];

    for (from, to, expected_namespace) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::BlankName { namespace, index: 0 }
                if namespace == expected_namespace
        ));
    }
}

#[test]
fn rejects_names_with_surrounding_whitespace_or_control_characters() {
    for name in [" web ", "web\\nedge"] {
        let error = error_from(&changed(
            "      name = \"web\",\n      bind",
            &format!("      name = \"{name}\",\n      bind"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidName {
                namespace: "listener",
                index: 0,
                ..
            }
        ));
    }
}

#[test]
fn rejects_duplicate_names_in_every_namespace() {
    let error = error_from(&changed(
        "      name = \"database\",\n      bind",
        "      name = \"web\",\n      bind",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "listener", name } if name == "web"
    ));

    let error = error_from(&changed(
        "      name = \"database-backends\",",
        "      name = \"web-backends\",",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "upstream pool", name }
            if name == "web-backends"
    ));

    let source = changed(
        "      max_request_body_bytes = 2097152,\n    },\n  },\n  l4_services = {",
        r#"      max_request_body_bytes = 2097152,
    },
    {
      name = "web",
      routes = { { upstream_pool = "web-backends" } },
    },
  },
  l4_services = {"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "HTTP service", name } if name == "web"
    ));

    let source = changed(
        "      lifetime_timeout_ms = 600000,\n    },",
        r#"      lifetime_timeout_ms = 600000,
    },
    {
      name = "database",
      upstream_pool = "database-backends",
    },"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "L4 service", name } if name == "database"
    ));
}

#[test]
fn rejects_overlapping_and_zero_port_listener_binds() {
    let error = error_from(&changed("127.0.0.1:15432", "127.0.0.1:8080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind {
            first_name,
            second_name,
            ..
        } if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "0.0.0.0:15432"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "127.0.0.1:9080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "management" && second_name == "web"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "[::ffff:127.0.0.1]:15432"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "[::ffff:127.0.0.1]:9080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "management" && second_name == "web"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "127.0.0.1:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "listener",
            name,
            field: "bind"
        } if name == "web"
    ));
}

#[test]
fn rejects_zero_listener_connection_limits() {
    let error = error_from(&changed(
        "      max_connections = 5000,",
        "      max_connections = 0,",
    ));

    assert!(matches!(
        error,
        ConfigError::ZeroLimit {
            kind: "listener",
            name,
            field: "max_connections"
        } if name == "web"
    ));
}

#[test]
fn rejects_listener_limits_that_json_cannot_represent_exactly() {
    let error = error_from(&changed(
        "      max_connections = 5000,",
        "      max_connections = 9007199254740992,",
    ));

    assert!(matches!(
        error,
        ConfigError::LimitTooLarge {
            kind: "listener",
            name,
            field: "max_connections"
        } if name == "web"
    ));
}

#[test]
fn requires_http_and_tcp_listener_services() {
    let cases = [
        ("      service = \"web\",\n", Protocol::Http, "web"),
        ("      service = \"database\",\n", Protocol::Tcp, "database"),
    ];

    for (field, protocol, listener) in cases {
        let error = error_from(&changed(field, ""));
        assert!(matches!(
            error,
            ConfigError::MissingListenerService {
                listener: actual_listener,
                protocol: actual_protocol,
            } if actual_listener == listener && actual_protocol == protocol
        ));
    }
}

#[test]
fn requires_http_and_tcp_listeners_to_reference_same_kind_services() {
    let cases = [
        (
            "      service = \"web\",",
            "      service = \"database\",",
            Protocol::Http,
            "web",
        ),
        (
            "      service = \"database\",",
            "      service = \"web\",",
            Protocol::Tcp,
            "database",
        ),
    ];

    for (from, to, protocol, listener) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::UnknownListenerService {
                listener: actual_listener,
                protocol: actual_protocol,
                ..
            } if actual_listener == listener && actual_protocol == protocol
        ));
    }
}

#[test]
fn rejects_a_service_reference_on_an_rtmp_listener() {
    let error = error_from(&changed(
        "      protocol = \"rtmp\",\n      max_connections",
        "      protocol = \"rtmp\",\n      service = \"web\",\n      max_connections",
    ));

    assert!(matches!(
        error,
        ConfigError::UnexpectedRtmpService { listener, service }
            if listener == "live" && service == "web"
    ));
}

#[test]
fn rejects_empty_duplicate_and_zero_port_pool_endpoints() {
    let error = error_from(&changed(
        "      endpoints = { \"10.0.0.12:5432\" },",
        "      endpoints = {},",
    ));
    assert!(matches!(
        error,
        ConfigError::EmptyUpstreamEndpoints { pool } if pool == "database-backends"
    ));

    let error = error_from(&changed(
        "      endpoints = { \"10.0.0.12:5432\" },",
        "      endpoints = { \"10.0.0.12:5432\", \"10.0.0.12:5432\" },",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "database-backends"
    ));

    let error = error_from(&changed("10.0.0.12:5432", "10.0.0.12:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "upstream pool",
            name,
            field: "endpoints"
        } if name == "database-backends"
    ));
}

#[test]
fn rejects_excessive_upstream_endpoint_cardinality() {
    let endpoints = (10_000..10_257)
        .map(|port| format!(r#""127.0.0.1:{port}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{ name = "oversized", endpoints = {{ {endpoints} }} }},
  }},
}}"#
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyUpstreamEndpoints { pool } if pool == "oversized"
    ));

    let pools = (0..5)
        .map(|pool| {
            let endpoints = (0..205)
                .map(|offset| {
                    let port = 20_000 + pool * 205 + offset;
                    format!(r#""127.0.0.1:{port}""#)
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(r#"{{ name = "pool-{pool}", endpoints = {{ {endpoints} }} }}"#)
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!(
        r"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {pools}
  }},
}}"
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyTotalUpstreamEndpoints
    ));
}

#[test]
fn rejects_a_pool_that_exposes_the_management_endpoint() {
    for endpoint in [
        "127.0.0.1:9080",
        "0.0.0.0:9080",
        "[::]:9080",
        "[::ffff:127.0.0.1]:9080",
        "[::ffff:0.0.0.0]:9080",
    ] {
        let error = error_from(&changed("10.0.0.12:5432", endpoint));
        assert!(matches!(
            error,
            ConfigError::ManagementUpstreamEndpoint { pool, .. }
                if pool == "database-backends"
        ));
    }
}

#[test]
fn rejects_empty_http_routes() {
    let source = changed(
        r#"      routes = {
        {
          host = "example.com",
          path_prefix = "/api",
          methods = { "GET", "POST" },
          upstream_pool = "web-backends",
        },
      },"#,
        "      routes = {},",
    );
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
            "          host = \"example.com\",",
            &format!("          host = \"{host}\","),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidRouteHost { service, route: 0, .. } if service == "web"
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
            "          path_prefix = \"/api\",",
            &format!("          path_prefix = \"{path_prefix}\","),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidRoutePathPrefix { service, route: 0, .. }
                if service == "web"
        ));
    }
}

#[test]
fn rejects_invalid_and_duplicate_route_methods() {
    for method in ["get", "GE T", "G\u{c9}T", ""] {
        let error = error_from(&changed(
            "          methods = { \"GET\", \"POST\" },",
            &format!("          methods = {{ \"{method}\" }},"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidRouteMethod { service, route: 0, .. } if service == "web"
        ));
    }

    let error = error_from(&changed(
        "          methods = { \"GET\", \"POST\" },",
        "          methods = { \"GET\", \"GET\" },",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateRouteMethod {
            service,
            route: 0,
            method
        } if service == "web" && method == "GET"
    ));
}

#[test]
fn rejects_duplicate_equivalent_routes_after_normalization() {
    let source = changed(
        "          upstream_pool = \"web-backends\",\n        },",
        r#"          upstream_pool = "web-backends",
        },
        {
          host = "EXAMPLE.COM",
          path_prefix = "/api",
          methods = { "POST", "GET" },
          upstream_pool = "database-backends",
        },"#,
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

#[test]
fn rejects_zero_http_service_limits() {
    let cases = [
        (
            "      upstream_io_timeout_ms = 15000,",
            "      upstream_io_timeout_ms = 0,",
            "upstream_io_timeout_ms",
        ),
        (
            "      max_request_body_bytes = 2097152,",
            "      max_request_body_bytes = 0,",
            "max_request_body_bytes",
        ),
    ];

    for (from, to, expected_field) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::ZeroLimit {
                kind: "HTTP service",
                name,
                field
            } if name == "web" && field == expected_field
        ));
    }
}

#[test]
fn rejects_zero_l4_service_timeouts() {
    let cases = [
        (
            "      connect_timeout_ms = 5000,",
            "      connect_timeout_ms = 0,",
            "connect_timeout_ms",
        ),
        (
            "      idle_timeout_ms = 120000,",
            "      idle_timeout_ms = 0,",
            "idle_timeout_ms",
        ),
        (
            "      lifetime_timeout_ms = 600000,",
            "      lifetime_timeout_ms = 0,",
            "lifetime_timeout_ms",
        ),
    ];

    for (from, to, expected_field) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::ZeroLimit {
                kind: "L4 service",
                name,
                field
            } if name == "database" && field == expected_field
        ));
    }
}

#[test]
fn rejects_unknown_fields_including_the_old_direct_upstream() {
    let source = changed(
        "      service = \"web\",",
        "      service = \"web\",\n      upstream = \"127.0.0.1:3000\",",
    );
    let error = error_from(&source);

    assert!(matches!(error, ConfigError::Lua(_)));
    assert!(error.to_string().contains("unknown field `upstream`"));
}

#[test]
fn rejects_unknown_protocols_and_algorithms() {
    let protocol_error = error_from(&changed(
        "      protocol = \"http\",",
        "      protocol = \"udp\",",
    ));
    assert!(matches!(protocol_error, ConfigError::Lua(_)));

    let algorithm_error = error_from(&changed(
        "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"round_robin\",",
        "      endpoints = { \"127.0.0.1:3000\", \"127.0.0.1:3001\" },\n      algorithm = \"least_connections\",",
    ));
    assert!(matches!(algorithm_error, ConfigError::Lua(_)));
}

#[test]
fn does_not_expose_operating_system_functions() {
    let source = r#"
os.execute("touch /tmp/oxiroute-lua-escaped")
return { version = 1, listeners = {} }
"#;
    let error = error_from(source);

    assert!(error.to_string().contains("os"));
    assert!(!std::path::Path::new("/tmp/oxiroute-lua-escaped").exists());
}

#[test]
fn enforces_the_source_size_limit() {
    let source = " ".repeat(1024 * 1024 + 1);
    let error = error_from(&source);

    assert!(matches!(error, ConfigError::SourceTooLarge));
}
