use oxiroute_config::{
    ConfigError, HealthCheckType, HttpVersion, UpstreamAlgorithm, UpstreamEndpoint,
    UpstreamTlsVerify,
};

use crate::common::{changed, error_from, with_web_pool_fields};
use crate::lua_support::{load_lua, load_lua_error, render_lua};

fn upstream_tls(server_name: &str) -> String {
    format!(
        r#"      tls = {{ server_name = "{server_name}", ca_certificate_path = "/etc/oxiroute/upstream-ca.pem" }},"#
    )
}

#[test]
fn loads_a_bounded_http_retry_budget() {
    for max_retries in [1, 2, 3] {
        let source = changed(
            "            policy = {},",
            &format!("            policy = {{ retry = {{ max_retries = {max_retries} }} }},"),
        );
        let config = load_lua(&source).expect("bounded retry budget");

        assert_eq!(
            serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["action"]
                ["policy"]["retry"]["max_retries"],
            max_retries
        );
    }
}

#[test]
fn rejects_an_excessive_http_retry_budget() {
    let source = changed(
        "            policy = {},",
        "            policy = { retry = { max_retries = 4 } },",
    );
    let error = error_from(&source);

    assert!(matches!(
        error,
        ConfigError::InvalidHttpRoute {
            service,
            route: 0,
            field: "action.policy.retry.max_retries",
            ..
        } if service == "web"
    ));
}

#[test]
fn loads_tcp_and_http_health_check_policies() {
    let tcp_source = with_web_pool_fields("      health_check = { type = \"tcp\" },");
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

    let http_source = with_web_pool_fields(
        r#"      health_check = {
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
fn loads_upstream_tls_and_all_supported_http_version_ranges() {
    for (min, max) in [("1.1", "1.1"), ("1.1", "2"), ("2", "2")] {
        let fields = format!(
            "{}\n      http_versions = {{ min = \"{min}\", max = \"{max}\" }},",
            upstream_tls("BACKEND.EXAMPLE.COM")
        );
        let source = with_web_pool_fields(&fields);
        let config = load_lua(&source).expect("supported upstream HTTP version range");
        let pool = &config.upstream_pools[0];
        let tls = pool.tls.as_ref().expect("upstream TLS");

        assert_eq!(tls.server_name, Some("backend.example.com".into()));
        assert_eq!(
            tls.ca_certificate_path.as_deref(),
            Some(std::path::Path::new("/etc/oxiroute/upstream-ca.pem"))
        );
        assert_eq!(
            pool.http_versions.min,
            if min == "1.1" {
                HttpVersion::Http11
            } else {
                HttpVersion::Http2
            }
        );
        assert_eq!(
            pool.http_versions.max,
            if max == "1.1" {
                HttpVersion::Http11
            } else {
                HttpVersion::Http2
            }
        );
    }

    let fields = format!(
        "{}\n      http_versions = {{ min = \"3\", max = \"3\" }},",
        upstream_tls("BACKEND.EXAMPLE.COM")
    );
    let source = with_web_pool_fields(&fields)
        .replacen(r#"alpn = { "h2", "http/1.1" },"#, r#"alpn = { "h3" },"#, 1)
        .replacen(
            r#"bind = { type = "socket", address = "127.0.0.1:8080" },"#,
            r#"bind = { type = "udp", address = "127.0.0.1:8080" },"#,
            1,
        )
        .replacen(r#"protocol = "http","#, r#"protocol = "http3","#, 1)
        .replacen(
            r#"path = { kind = "segment_prefix", value = "/api" },"#,
            r#"path = { kind = "segment_prefix", value = "/api" },
          policy = { request_buffering = true },"#,
            1,
        );
    let config = load_lua(&source).expect("HTTP/3 upstream range");
    assert_eq!(
        config.upstream_pools[0].http_versions.min,
        HttpVersion::Http3
    );
    assert_eq!(
        config.upstream_pools[0].http_versions.max,
        HttpVersion::Http3
    );
}

#[test]
fn validates_upstream_ca_paths_lexically() {
    let path = format!("/{}", "a".repeat(4_095));
    let fields = format!(
        r#"      tls = {{ server_name = "backend.example.com", ca_certificate_path = "{path}" }},"#
    );
    load_lua(&with_web_pool_fields(&fields)).expect("4096-byte upstream CA path");

    for path in [
        format!("/{}", "a".repeat(4_096)),
        "etc/oxiroute/ca.pem".into(),
        "/etc//ca.pem".into(),
        "/etc/ca/".into(),
        "/etc/./ca.pem".into(),
        "/etc/../ca.pem".into(),
        r"/etc/\0ca.pem".into(),
    ] {
        let fields = format!(
            r#"      tls = {{ server_name = "backend.example.com", ca_certificate_path = "{path}" }},"#
        );
        assert!(matches!(
            error_from(&with_web_pool_fields(&fields)),
            ConfigError::InvalidFilePath {
                kind: "upstream pool",
                field: "tls.ca_certificate_path",
                ..
            }
        ));
    }
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_file_paths() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

    use oxiroute_config::{
        DnsResolutionPolicy, HttpVersionPolicy, UpstreamConnectionReuse, UpstreamPool,
        UpstreamServer, UpstreamTls, validate_upstream_pool_definitions,
    };

    let pool = UpstreamPool {
        name: "secure".into(),
        servers: vec![UpstreamServer {
            name: "secure-1".into(),
            endpoint: UpstreamEndpoint::Socket {
                address: "127.0.0.1:443".parse().expect("endpoint"),
            },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: Some("backend.example.com".into()),
            verify: oxiroute_config::UpstreamTlsVerify::Full,
            ca_certificate_path: Some(PathBuf::from(OsString::from_vec(
                b"/etc/oxiroute/ca-\xff.pem".to_vec(),
            ))),
        }),
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::Safe,
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    };

    assert!(matches!(
        validate_upstream_pool_definitions(&[pool], None),
        Err(ConfigError::InvalidFilePath {
            kind: "upstream pool",
            field: "tls.ca_certificate_path",
            detail: "path must be valid UTF-8",
            ..
        })
    ));
}

#[test]
fn validates_upstream_tls_server_names() {
    let boundary_name = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    let config = load_lua(&with_web_pool_fields(&upstream_tls(&boundary_name)))
        .expect("253-byte DNS server name");
    assert_eq!(
        config.upstream_pools[0]
            .tls
            .as_ref()
            .expect("upstream TLS")
            .server_name,
        Some(boundary_name)
    );

    let too_long = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(62)
    );
    for server_name in [
        String::new(),
        "*.example.com".into(),
        "127.0.0.1".into(),
        "::1".into(),
        "example.com.".into(),
        "-api.example.com".into(),
        "api-.example.com".into(),
        "api..example.com".into(),
        "caf\u{e9}.example.com".into(),
        format!("{}.example.com", "a".repeat(64)),
        too_long,
    ] {
        assert!(matches!(
            error_from(&with_web_pool_fields(&upstream_tls(&server_name))),
            ConfigError::InvalidUpstreamTlsServerName { pool, .. }
                if pool == "web-backends"
        ));
    }
}

#[test]
fn rejects_invalid_or_plaintext_http2_upstream_ranges() {
    let fields = format!(
        "{}\n      http_versions = {{ min = \"2\", max = \"1.1\" }},",
        upstream_tls("backend.example.com")
    );
    assert!(matches!(
        error_from(&with_web_pool_fields(&fields)),
        ConfigError::InvalidHttpVersionRange {
            pool,
            min: "2",
            max: "1.1"
        } if pool == "web-backends"
    ));

    for min in ["1.1", "2"] {
        let fields = format!("      http_versions = {{ min = \"{min}\", max = \"2\" }},");
        assert!(matches!(
            error_from(&with_web_pool_fields(&fields)),
            ConfigError::H2RequiresUpstreamTls { pool } if pool == "web-backends"
        ));
    }

    let fields = r#"      http_versions = { min = "3", max = "3" },"#;
    assert!(matches!(
        error_from(&with_web_pool_fields(fields)),
        ConfigError::H3RequiresUpstreamTls { pool } if pool == "web-backends"
    ));
    let fields = format!(
        "{}\n      http_versions = {{ min = \"2\", max = \"3\" }},",
        upstream_tls("backend.example.com")
    );
    assert!(matches!(
        error_from(&with_web_pool_fields(&fields)),
        ConfigError::InvalidHttpVersionRange {
            pool,
            min: "2",
            max: "3"
        } if pool == "web-backends"
    ));
}

#[test]
fn rejects_health_checks_combined_with_upstream_tls() {
    let fields = format!(
        "{}\n      health_check = {{ type = \"tcp\" }},",
        upstream_tls("backend.example.com")
    );
    let error = error_from(&with_web_pool_fields(&fields));

    assert!(matches!(
        &error,
        ConfigError::UnsupportedTlsHealthCheck { pool } if pool == "web-backends"
    ));
    assert!(
        error
            .to_string()
            .contains("combines `health_check` with `tls`, which is not supported")
    );
}

#[test]
fn rejects_l4_references_to_tls_enabled_upstream_pools() {
    let pool = r#"      endpoints = { { type = "socket", address = "192.0.2.58:5432" } },
      algorithm = "round_robin","#;
    let source = changed(
        pool,
        &format!("{pool}\n      tls = {{ server_name = \"database.example.com\" }},"),
    );

    assert!(matches!(
        error_from(&source),
        ConfigError::TlsUpstreamPoolForL4Service { service, pool }
            if service == "database" && pool == "database-backends"
    ));
}

#[test]
fn rejects_invalid_health_check_timing_and_thresholds() {
    for policy in [
        r#"{ type = "tcp", interval_ms = 999 }"#,
        r#"{ type = "tcp", interval_ms = 86400001 }"#,
        r#"{ type = "tcp", interval_ms = 40000, timeout_ms = 30001 }"#,
        r#"{ type = "tcp", healthy_threshold = 0 }"#,
        r#"{ type = "tcp", healthy_threshold = 101 }"#,
        r#"{ type = "tcp", unhealthy_threshold = 0 }"#,
        r#"{ type = "tcp", unhealthy_threshold = 101 }"#,
    ] {
        let source = with_web_pool_fields(&format!("      health_check = {policy},"));
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }
}

#[test]
fn health_check_timeout_may_equal_its_interval() {
    let source = with_web_pool_fields(
        r#"      health_check = { type = "tcp", interval_ms = 10000, timeout_ms = 10000 },"#,
    );
    let config = load_lua(&source).expect("equal health interval and timeout");
    let health = config.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("health check");

    assert_eq!(health.interval_ms, 10_000);
    assert_eq!(health.timeout_ms, 10_000);
}

#[test]
fn rejects_health_check_fields_that_do_not_match_the_probe_type() {
    load_lua(&with_web_pool_fields(
        r#"      health_check = { type = "http", path = "/healthz" },"#,
    ))
    .expect("HTTP health-check Host policy is optional");

    for policy in [
        r#"{ type = "http", host = "backend.internal" }"#,
        r#"{ type = "http", host = "user@backend.internal", path = "/healthz" }"#,
        r#"{ type = "http", host = "backend.internal:not-a-port", path = "/healthz" }"#,
        r#"{ type = "http", host = "backend.internal", path = "healthz" }"#,
        r#"{ type = "http", host = "backend.internal", path = "/healthz?full=true" }"#,
        r#"{ type = "tcp", host = "backend.internal" }"#,
        r#"{ type = "tcp", path = "/healthz" }"#,
    ] {
        let source = with_web_pool_fields(&format!("      health_check = {policy},"));
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
        let source = with_web_pool_fields(&format!("      health_check = {policy},"));
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }
}

#[test]
fn rejects_empty_duplicate_and_zero_port_pool_endpoints() {
    let error = error_from(&changed(
        "      endpoints = { { type = \"socket\", address = \"192.0.2.58:5432\" } },",
        "      endpoints = {},",
    ));
    assert!(matches!(
        error,
        ConfigError::EmptyUpstreamEndpoints { pool } if pool == "database-backends"
    ));

    let error = error_from(&changed(
        "      endpoints = { { type = \"socket\", address = \"192.0.2.58:5432\" } },",
        "      endpoints = { { type = \"socket\", address = \"192.0.2.58:5432\" }, { type = \"socket\", address = \"192.0.2.58:5432\" } },",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "database-backends"
    ));

    let error = error_from(&changed("192.0.2.58:5432", "192.0.2.58:0"));
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
fn loads_and_round_trips_weighted_round_robin_policy() {
    let config = load_lua(
        r#"return {
  version = 1,
  listeners = {},
  upstream_pools = {
    {
      name = "weighted",
      servers = {
        { name = "primary", endpoint = { type = "socket", address = "127.0.0.1:3000" } },
        { name = "backup", endpoint = { type = "socket", address = "127.0.0.1:3001" } },
      },
      algorithm = { type = "weighted_round_robin", weights = { 3, 1 } },
    },
  },
}"#,
    )
    .expect("weighted round-robin policy");
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1]
        }
    );

    let rendered = render_lua(&config).expect("weighted round-robin render");
    assert!(rendered.contains("type = \"weighted_round_robin\""));
    assert_eq!(
        load_lua(&rendered).expect("weighted round-robin reload"),
        config
    );
}

#[test]
fn rejects_weighted_round_robin_with_missing_zero_or_oversized_weights() {
    let sources = [r"{ 3 }", r"{ 3, 0 }", r"{ 3, 101 }"];
    for weights in sources {
        let source = format!(
            r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{
      name = "weighted",
      servers = {{
        {{ name = "primary", endpoint = {{ type = "socket", address = "127.0.0.1:3000" }} }},
        {{ name = "backup", endpoint = {{ type = "socket", address = "127.0.0.1:3001" }} }},
      }},
      algorithm = {{ type = "weighted_round_robin", weights = {weights} }},
    }},
  }},
}}"#
        );
        assert!(matches!(
            load_lua(&source).expect_err("invalid weighted round-robin policy"),
            ConfigError::InvalidUpstreamWeights { pool, .. } if pool == "weighted"
        ));
    }
}

#[test]
fn rejects_excessive_upstream_endpoint_cardinality() {
    let endpoints = (10_000..10_257)
        .map(|port| format!(r#"{{ type = "socket", address = "127.0.0.1:{port}" }}"#))
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
                    format!(r#"{{ type = "socket", address = "127.0.0.1:{port}" }}"#)
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
        let error = error_from(&changed("192.0.2.58:5432", endpoint));
        assert!(matches!(
            error,
            ConfigError::ManagementUpstreamEndpoint { pool, .. }
                if pool == "database-backends"
        ));
    }
}

#[test]
fn verify_none_upstream_tls_needs_no_server_name_and_warns() {
    let config = load_lua(&with_web_pool_fields(r#"      tls = { verify = "none" },"#))
        .expect("unverified upstream TLS");
    let tls = config.upstream_pools[0].tls.as_ref().expect("upstream TLS");
    assert_eq!(tls.verify, UpstreamTlsVerify::None);
    assert_eq!(tls.server_name, None);
    let validated = config.clone().validate().expect("validated");
    assert_eq!(
        validated
            .warnings()
            .iter()
            .map(|warning| warning.code)
            .collect::<Vec<_>>(),
        [oxiroute_config::W_UPSTREAM_TLS_UNVERIFIED]
    );
    let rendered = render_lua(&config).expect("render unverified upstream TLS");
    assert_eq!(load_lua(&rendered).expect("roundtrip"), config);

    let sni = load_lua(&with_web_pool_fields(
        r#"      tls = { verify = "none", server_name = "Origin.Example.Test" },"#,
    ))
    .expect("unverified upstream TLS with SNI");
    let tls = sni.upstream_pools[0].tls.as_ref().expect("upstream TLS");
    assert_eq!(tls.server_name.as_deref(), Some("origin.example.test"));

    let verified = load_lua(&with_web_pool_fields(&upstream_tls("backend.example.com")))
        .expect("verified upstream TLS");
    assert_eq!(
        verified.upstream_pools[0].tls.as_ref().expect("TLS").verify,
        UpstreamTlsVerify::Full
    );
    assert!(
        verified
            .validate()
            .expect("validated")
            .warnings()
            .is_empty()
    );
}

#[test]
fn rejects_inconsistent_upstream_tls_verify_settings() {
    for (fields, detail) in [
        ("      tls = {},", "server_name is required"),
        (
            r#"      tls = { verify = "full" },"#,
            "server_name is required",
        ),
        (
            r#"      tls = { verify = "none", ca_certificate_path = "/etc/oxiroute/upstream-ca.pem" },"#,
            "ca_certificate_path requires verify full",
        ),
        (
            "      tls = { verify = \"none\" },\n      http_versions = { min = \"3\", max = \"3\" },",
            "does not support HTTP/3",
        ),
    ] {
        match error_from(&with_web_pool_fields(fields)) {
            ConfigError::InvalidUpstreamTls {
                pool,
                detail: actual,
            } => {
                assert_eq!(pool, "web-backends");
                assert!(actual.contains(detail), "{fields}: {actual}");
            }
            other => panic!("{fields}: {other}"),
        }
    }
    assert!(matches!(
        error_from(&with_web_pool_fields(
            r#"      tls = { verify = "none", server_name = "192.0.2.1" },"#
        )),
        ConfigError::InvalidUpstreamTlsServerName { .. }
    ));
    let unknown = load_lua_error(&with_web_pool_fields(
        r#"      tls = { verify = "peer", server_name = "origin.example.test" },"#,
    ));
    assert!(
        unknown.to_string().contains("unknown variant `peer`"),
        "{unknown}"
    );
}
