use oxiroute_config::{ConfigError, ListenerBind, UpstreamAlgorithm, UpstreamEndpoint};

use crate::common::{changed, error_from};
use crate::lua_support::load_lua;

#[test]
fn loads_and_normalizes_every_bind_and_endpoint_variant() {
    let source = r#"
return {
  version = 1,
  listeners = {
    {
      name = "local",
      bind = { type = "unix", path = "/run//oxiroute///local.sock" },
      protocol = "rtmp",
      service = "live",
      max_connections = null,
    },
  },
  upstream_pools = {
    {
      name = "all-endpoints",
      endpoints = {
        { type = "socket", address = "[::ffff:127.0.0.1]:3000" },
        { type = "dns", host = "BACKEND-1.EXAMPLE.TEST", port = 3001 },
        { type = "unix", path = "/run//oxiroute///backend.sock" },
      },
      algorithm = "least_connections",
    },
  },
  rtmp_services = {
    { name = "live", applications = { { name = "live" } } },
  },
}
"#;
    let config = load_lua(source).expect("all bind and endpoint variants");

    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/run/oxiroute/local.sock".into(),
            mode: None,
        }
    );
    assert_eq!(config.listeners[0].max_connections, None);
    assert_eq!(
        config.upstream_pools[0]
            .servers
            .iter()
            .map(|server| server.endpoint.clone())
            .collect::<Vec<_>>(),
        [
            UpstreamEndpoint::Socket {
                address: "127.0.0.1:3000".parse().expect("socket address")
            },
            UpstreamEndpoint::Dns {
                host: "backend-1.example.test".into(),
                port: 3001
            },
            UpstreamEndpoint::Unix {
                path: "/run/oxiroute/backend.sock".into()
            },
        ]
    );
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::LeastConnections
    );
}

#[test]
fn validates_and_normalizes_dns_endpoints_without_resolution_or_expansion() {
    let boundary = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{ name = "dns", endpoints = {{ {{ type = "dns", host = "{boundary}", port = 443 }} }} }},
  }},
}}"#
    );
    let config = load_lua(&source).expect("253-byte DNS endpoint");
    assert!(matches!(
        &config.upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Dns { host, port: 443 } if host == &boundary
    ));

    let too_long = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(62)
    );
    for host in [
        String::new(),
        "127.0.0.1".into(),
        "::1".into(),
        "example.test.".into(),
        "-api.example.test".into(),
        "api-.example.test".into(),
        "api..example.test".into(),
        "api_example.test".into(),
        "caf\u{e9}.example.test".into(),
        "${BACKEND_HOST}".into(),
        format!("{}.example.test", "a".repeat(64)),
        too_long,
    ] {
        let source = format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "dns", endpoints = {{ {{ type = "dns", host = "{host}", port = 443 }} }} }},
}} }}"#
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidDnsEndpoint { pool, .. } if pool == "dns"
        ));
    }

    let zero_port = r#"return { version = 1, listeners = {}, upstream_pools = {
  { name = "dns", endpoints = { { type = "dns", host = "backend.test", port = 0 } } },
} }"#;
    assert!(matches!(
        error_from(zero_port),
        ConfigError::ZeroPort { kind: "upstream pool", name, field: "endpoints" }
            if name == "dns"
    ));
}

#[test]
fn validates_normalizes_and_deduplicates_unix_paths() {
    let boundary = format!("/{}", "a".repeat(106));
    let source = format!(
        r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "unix", endpoints = {{ {{ type = "unix", path = "{boundary}" }} }} }},
}} }}"#
    );
    load_lua(&source).expect("107-byte Unix path");

    for path in [
        "run/backend.sock".to_owned(),
        "/".to_owned(),
        "/run/backend.sock/".to_owned(),
        "/run/./backend.sock".to_owned(),
        "/run/../backend.sock".to_owned(),
        r"/run/\0backend.sock".to_owned(),
        format!("/{}", "a".repeat(107)),
    ] {
        let source = format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "unix", endpoints = {{ {{ type = "unix", path = "{path}" }} }} }},
}} }}"#
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidUnixPath { kind: "upstream pool", name, .. }
                if name == "unix"
        ));
    }

    let duplicates = r#"return { version = 1, listeners = {}, upstream_pools = {
  { name = "unix", endpoints = {
    { type = "unix", path = "/run/oxiroute/backend.sock" },
    { type = "unix", path = "/run//oxiroute///backend.sock" },
  } },
} }"#;
    assert!(matches!(
        error_from(duplicates),
        ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "unix"
    ));
}

#[test]
fn rejects_duplicate_normalized_socket_and_dns_endpoints() {
    for endpoints in [
        r#"{ type = "socket", address = "127.0.0.1:3000" },
    { type = "socket", address = "[::ffff:127.0.0.1]:3000" }"#,
        r#"{ type = "dns", host = "BACKEND.EXAMPLE.TEST", port = 3000 },
    { type = "dns", host = "backend.example.test", port = 3000 }"#,
    ] {
        let source = format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "duplicates", endpoints = {{ {endpoints} }} }},
}} }}"#
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "duplicates"
        ));
    }
}

#[test]
fn rejects_duplicate_normalized_unix_listener_binds() {
    let source = r#"
return {
  version = 1,
  listeners = {
    { name = "first", bind = { type = "unix", path = "/run/oxiroute/live.sock" }, protocol = "rtmp", service = "live" },
    { name = "second", bind = { type = "unix", path = "/run//oxiroute///live.sock" }, protocol = "rtmp", service = "live" },
  },
  rtmp_services = { { name = "live", applications = { { name = "live" } } } },
}
"#;
    assert!(matches!(
        error_from(source),
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "first" && second_name == "second"
    ));
}

#[test]
fn limits_tls_and_health_checks_to_supported_endpoint_transports() {
    let unix_listener = changed(
        r#"      bind = { type = "socket", address = "127.0.0.1:8080" },"#,
        r#"      bind = { type = "unix", path = "/run/oxiroute/web.sock" },"#,
    );
    assert!(matches!(
        error_from(&unix_listener),
        ConfigError::UnsupportedUnixListenerTls { listener, profile }
            if listener == "web" && profile == "web-tls"
    ));

    let unix_pool = |extra: &str| {
        format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{
    name = "unix",
    endpoints = {{ {{ type = "unix", path = "/run/oxiroute/backend.sock" }} }},
    {extra}
  }},
}} }}"#
        )
    };
    assert!(matches!(
        error_from(&unix_pool(r#"tls = { server_name = "backend.example.test" },"#)),
        ConfigError::UnsupportedUnixUpstreamTls { pool } if pool == "unix"
    ));
    for health_check in [
        r#"health_check = { type = "tcp" },"#,
        r#"health_check = { type = "http", host = "backend", path = "/healthz" },"#,
    ] {
        assert!(matches!(
            error_from(&unix_pool(health_check)),
            ConfigError::UnsupportedUnixHealthCheck { pool } if pool == "unix"
        ));
    }

    for endpoint in [
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        r#"{ type = "dns", host = "backend.example.test", port = 3000 }"#,
    ] {
        for health_check in [
            r#"{ type = "tcp" }"#,
            r#"{ type = "http", host = "backend.example.test", path = "/healthz" }"#,
        ] {
            let source = format!(
                r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "supported", endpoints = {{ {endpoint} }}, health_check = {health_check} }},
}} }}"#
            );
            load_lua(&source).expect("socket and DNS health checks are supported");
        }
    }
}

#[test]
fn management_exposure_checks_only_socket_upstreams() {
    let source = r#"
return {
  version = 1,
  management = { bind = "127.0.0.1:9080" },
  listeners = {},
  upstream_pools = {
    {
      name = "non-socket",
      endpoints = {
        { type = "dns", host = "localhost", port = 9080 },
        { type = "unix", path = "/run/oxiroute/management.sock" },
      },
    },
  },
}
"#;
    load_lua(source).expect("DNS is not resolved and Unix has a distinct identity");
}
