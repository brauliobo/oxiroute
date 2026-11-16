use std::{
    fs,
    net::{IpAddr, Ipv4Addr},
};

use oxiroute_config::{HttpRouteAction, ListenerBind, UpstreamAlgorithm, UpstreamEndpoint};
use oxiroute_import::{
    Diagnostic, Severity,
    haproxy::{
        CanonicalCandidate, E_PROCESS_OWNED, HaproxyImportOptions,
        HaproxyOneRequestPerConnectionOverlay, PreprocessingEnvironment, import_roots_with_options,
    },
};
use tempfile::tempdir;

use super::common::{
    MINIMAL, assert_blocker, assert_has_provenance, code_count, diagnostic_contains,
    import_fixture, import_synthetic, validated_config,
};

const SYNTHETIC_UNIX_DNS_LEASTCONN: &[u8] =
    include_bytes!("../../fixtures/haproxy/synthetic-unix-dns-leastconn.cfg");

const HTTP_CHECK_SEND: &[u8] = include_bytes!("../../fixtures/haproxy/http-check-send.cfg");

#[test]
fn dns_leastconn_pool_finalizes_with_reusable_connections_and_queue_timeout() {
    let lowered = import_synthetic("dns-leastconn-pool.cfg", false);
    let candidate = lowered.value();
    let config = validated_config(candidate);

    assert_eq!(candidate.summary().upstream_pools, 1);
    assert_eq!(candidate.summary().listeners, 1);
    assert_eq!(candidate.summary().max_connections, Some(2048));
    assert_eq!(
        config.upstream_pools[0].connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Safe
    );
    assert_eq!(config.upstream_pools[0].queue_timeout_ms, Some(600_000));
    assert_eq!(code_count(lowered.diagnostics(), E_PROCESS_OWNED), 4);
    assert_process_settings_are_external_warnings(lowered.diagnostics());
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "DNS-named servers"
    ));
    assert_no_fallback_routes(candidate);
}

#[test]
fn audited_connection_lifecycle_overlay_is_backend_scoped_and_fail_closed() {
    let directory = tempdir().expect("HAProxy lifecycle overlay directory");
    let root = directory.path().join("haproxy.cfg");
    fs::write(
        &root,
        b"defaults web\n  mode http\n  retries 0\n  timeout connect 5s\n  timeout client 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18080 maxconn 10\n  maxconn 10\n  default_backend app\nbackend app\n  balance leastconn\n  server app1 127.0.0.1:3000 maxconn 2\n",
    )
    .expect("write lifecycle overlay fixture");
    let environment = PreprocessingEnvironment {
        node_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        gpu1_defined: false,
    };
    let options = HaproxyImportOptions {
        one_request_per_connection: vec![HaproxyOneRequestPerConnectionOverlay {
            backend: "app".into(),
        }],
        prometheus_migrations: Vec::new(),
    };

    let imported = import_roots_with_options(&[&root], environment, &options);
    let candidate = imported.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("audited lifecycle config");
    assert_eq!(
        config.upstream_pools[0].connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Never
    );
    assert!(candidate.operational_overlays[0].satisfied);

    let wrong_backend = import_roots_with_options(
        &[&root],
        environment,
        &HaproxyImportOptions {
            one_request_per_connection: vec![HaproxyOneRequestPerConnectionOverlay {
                backend: "other".into(),
            }],
            prometheus_migrations: Vec::new(),
        },
    );
    assert!(
        wrong_backend
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(!wrong_backend.value().operational_overlays[0].satisfied);
}

#[test]
fn ordered_default_server_health_options_materialize_on_subsequent_servers() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout client 30s
  timeout server 30s
  default-server check inter 10s fastinter 3s
  default-server inter 20s downinter 1m fall 3 rise 2
frontend public
  bind 127.0.0.1:18080 maxconn 10
  maxconn 10
  default_backend app
backend app
  balance roundrobin
  option httpchk GET /health
  http-check expect status 200
  server app1 127.0.0.1:3000
  server app2 127.0.0.1:3001
";

    let imported = import_fixture("ordered-default-server.cfg", source);
    let health = imported
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("default-server config")
        .upstream_pools[0]
        .health_check
        .as_ref()
        .expect("materialized health check");
    assert_eq!(health.interval_ms, 20_000);
    assert_eq!(health.fast_interval_ms, Some(3_000));
    assert_eq!(health.down_interval_ms, Some(60_000));
    assert_eq!(health.healthy_threshold, 2);
    assert_eq!(health.unhealthy_threshold, 3);
}

#[test]
fn bounded_server_weights_lower_with_effective_defaults_and_provenance() {
    let source = b"defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout queue 15s
  timeout client 5m
  timeout server 5m
  default-server weight 20
frontend postgres
  bind 127.0.0.1:15432
  maxconn 1000
  default_backend postgres_pool
backend postgres_pool
  balance roundrobin
  server primary 127.0.0.1:5432 weight 50
  server secondary 127.0.0.1:5433
";
    let imported = import_fixture("weighted-servers.cfg", source);
    let candidate = imported.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("weighted config");

    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![50, 20]
        }
    );
    assert_has_provenance(candidate, "/upstream_pools/0/algorithm/weights/0");
    assert_has_provenance(candidate, "/upstream_pools/0/algorithm/weights/1");
}

#[test]
fn weighted_servers_require_roundrobin_balance() {
    let source = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace("balance roundrobin", "balance leastconn")
        .replace(
            "server primary 127.0.0.1:5432",
            "server primary 127.0.0.1:5432 weight 50",
        );
    let imported = import_fixture("weighted-leastconn.cfg", source.as_bytes());

    assert!(
        imported
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_blocker(
        imported.diagnostics(),
        "representable only with roundrobin balance",
    );
}

#[test]
fn http_check_send_lowers_to_the_existing_runtime_health_request() {
    let imported = import_fixture("http-check-send.cfg", HTTP_CHECK_SEND);
    assert!(!imported.has_errors(), "{:?}", imported.diagnostics());
    let candidate = imported.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("send health config");
    let health = config.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("lowered HTTP health check");

    assert_eq!(health.kind, oxiroute_config::HealthCheckType::Http);
    assert_eq!(health.interval_ms, 1_000);
    assert_eq!(health.timeout_ms, 1_000);
    assert_eq!(health.healthy_threshold, 1);
    assert_eq!(health.unhealthy_threshold, 2);
    assert_eq!(health.host.as_deref(), Some("backend.internal"));
    assert_eq!(health.path.as_deref(), Some("/healthz"));
    assert_eq!(health.expected_status, Some(204));
    assert_eq!(
        health.http_version,
        Some(oxiroute_config::HealthHttpVersion::Http11)
    );
    for path in [
        "/upstream_pools/0/health_check/kind",
        "/upstream_pools/0/health_check/path",
        "/upstream_pools/0/health_check/host",
        "/upstream_pools/0/health_check/http_version",
        "/upstream_pools/0/health_check/expected_status",
        "/upstream_pools/0/health_check/interval_ms",
        "/upstream_pools/0/health_check/timeout_ms",
        "/upstream_pools/0/health_check/healthy_threshold",
        "/upstream_pools/0/health_check/unhealthy_threshold",
    ] {
        assert_has_provenance(candidate, path);
    }
}

#[test]
fn http_check_send_with_disabled_http_checks_fails_closed() {
    let source = String::from_utf8(HTTP_CHECK_SEND.to_vec())
        .expect("HTTP check fixture UTF-8")
        .replace("  option httpchk\n", "  no option httpchk\n");
    let imported = import_fixture("disabled-http-check-send.cfg", source.as_bytes());

    assert!(
        imported
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(imported.value().summary().upstream_pools, 0);
    assert!(diagnostic_contains(
        imported.diagnostics(),
        "cannot be combined with disabled option httpchk"
    ));
}

#[test]
fn http_check_send_does_not_mask_unrepresented_native_policy() {
    for (name, frontend_policy, backend_policy) in [
        (
            "acl-expression",
            "  use_backend app if { path /healthz }\n",
            "",
        ),
        (
            "map-expression",
            "  http-request set-header X-Route %[map_str(routes.map)]\n",
            "",
        ),
        ("lua", "", "  lua.health\n"),
        ("spoe", "", "  filter spoe engine audit\n"),
    ] {
        let source = format!(
            "defaults web\n  mode http\n  timeout connect 5s\n  timeout client 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18080\n  default_backend app\n{frontend_policy}backend app\n{backend_policy}  balance roundrobin\n  http-check send meth GET uri /healthz ver HTTP/1.1\n  http-check expect status 204\n  server app1 127.0.0.1:3000 check\n"
        );
        let imported = import_fixture(name, source.as_bytes());

        assert!(
            imported
                .value()
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "{name} finalized"
        );
        assert!(imported.has_errors(), "{name} was not blocked");
    }
}

#[test]
fn health_without_timeout_check_preserves_interval_as_exact_timeout() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout server 600s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  option httpchk GET /
  http-check expect status 200
  server app1 127.0.0.1:3000 check inter 10s fall 3 rise 2
";
    let imported = import_fixture("equal-health-timeout.cfg", source);
    let health = imported
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("health config")
        .upstream_pools[0]
        .health_check
        .as_ref()
        .expect("health check");

    assert_eq!(health.interval_ms, 10_000);
    assert_eq!(health.timeout_ms, 10_000);
}

#[test]
fn non_get_http_health_checks_fail_closed() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout client 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080 maxconn 10
  maxconn 10
  default_backend app
backend app
  balance roundrobin
  option httpchk POST /health
  http-check expect status 200
  default-server check inter 10s
  server app1 127.0.0.1:3000
";

    let imported = import_fixture("post-health-check.cfg", source);

    assert!(
        imported
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_blocker(imported.diagnostics(), "health check method");
}

#[test]
fn fractional_millisecond_durations_fail_closed_at_their_source() {
    for (name, defaults_directive, client_timeout, health_interval) in [
        ("queue", "timeout queue 1500us", "30s", "10s"),
        ("health interval", "", "30s", "1500us"),
        ("client", "", "1500us", "10s"),
    ] {
        let source = format!(
            "defaults web\n  mode http\n  retries 0\n  timeout connect 5s\n  timeout client {client_timeout}\n  timeout server 30s\n{defaults_directive}\nfrontend public\n  bind 127.0.0.1:18080 maxconn 10\n  maxconn 10\n  default_backend app\nbackend app\n  balance roundrobin\n  option httpchk GET /health\n  http-check expect status 200\n  default-server check inter {health_interval}\n  server app1 127.0.0.1:3000\n"
        );
        let imported = import_fixture("fractional-duration.cfg", source.as_bytes());

        assert!(
            imported
                .value()
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "{name} was truncated"
        );
        assert!(
            diagnostic_contains(imported.diagnostics(), "exactly representable"),
            "{name}: {:?}",
            imported.diagnostics()
        );
    }
}

#[test]
fn audited_shape_unix_frontend_and_dns_leastconn_backend_finalizes_without_resolution() {
    let lowered = import_fixture(
        "synthetic-unix-dns-leastconn.cfg",
        SYNTHETIC_UNIX_DNS_LEASTCONN,
    );

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let candidate = lowered.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized edge-router subset");
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/run/haproxy/edge-router.sock".into(),
            mode: None,
        }
    );
    assert_eq!(config.listeners[0].max_connections, Some(1500));
    assert_eq!(
        config.upstream_pools[0]
            .servers
            .iter()
            .map(|server| server.endpoint.clone())
            .collect::<Vec<_>>(),
        [
            UpstreamEndpoint::Dns {
                host: "unresolvable-app01.invalid".into(),
                port: 3000,
            },
            UpstreamEndpoint::Dns {
                host: "unresolvable-app02.invalid".into(),
                port: 3000,
            },
        ]
    );
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::LeastConnections
    );
    for path in [
        "/listeners/0/bind/type",
        "/listeners/0/bind/path",
        "/listeners/0/max_connections",
        "/upstream_pools/0/servers/0/endpoint/type",
        "/upstream_pools/0/servers/0/endpoint/host",
        "/upstream_pools/0/servers/0/endpoint/port",
        "/upstream_pools/0/algorithm",
    ] {
        assert_has_provenance(candidate, path);
    }
}

#[test]
fn absolute_unix_server_lowers_without_socket_substitution() {
    let source = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace(
            "server primary 127.0.0.1:5432",
            "server primary /run/postgresql/.s.PGSQL.5432",
        );
    let lowered = import_fixture("unix-server.cfg", source.as_bytes());
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized Unix pool");

    assert_eq!(
        config.upstream_pools[0]
            .servers
            .iter()
            .map(|server| server.endpoint.clone())
            .collect::<Vec<_>>(),
        [UpstreamEndpoint::Unix {
            path: "/run/postgresql/.s.PGSQL.5432".into()
        }]
    );
    assert_has_provenance(lowered.value(), "/upstream_pools/0/servers/0/endpoint/path");
}

#[test]
fn leastconn_lowers_for_http_connection_accounting() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  maxconn 100
  default_backend app
backend app
  balance leastconn
  server app1 127.0.0.1:3000
  server app2 127.0.0.1:3001
";
    let lowered = import_fixture("http-leastconn.cfg", source);

    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("HTTP leastconn config");
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::LeastConnections
    );
    assert_eq!(
        config.upstream_pools[0].connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Safe
    );
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(config.listeners.len(), 1);
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "request-body limit"
    ));
}

#[test]
fn first_and_server_maxconn_still_require_request_lifetime_connections() {
    for backend in [
        "balance first\n  server app1 127.0.0.1:3000",
        "balance roundrobin\n  server app1 127.0.0.1:3000 maxconn 2",
    ] {
        let source = format!(
            "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18080\n  default_backend app\nbackend app\n  {backend}\n"
        );
        let lowered = import_fixture("request-lifetime-sensitive.cfg", source.as_bytes());

        assert!(
            lowered
                .value()
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "{backend}"
        );
        assert_blocker(lowered.diagnostics(), "server maxconn/first");
    }
}

#[test]
fn server_selection_options_remain_blocking_during_safe_import() {
    let source = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace(
            "server primary 127.0.0.1:5432",
            "server primary 127.0.0.1:5432 weight 50 backup maxconn 10 ssl verify required",
        );
    let lowered = import_fixture("server-options.cfg", source.as_bytes());

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(lowered.value().summary().upstream_pools, 0);
    assert_blocker(
        lowered.diagnostics(),
        "server selection, capacity, TLS, or check option",
    );
}

fn assert_process_settings_are_external_warnings(diagnostics: &[Diagnostic]) {
    assert!(
        diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code() == E_PROCESS_OWNED
                    && diagnostic.severity() == Severity::Warning
                    && diagnostic.primary_span().is_some()
            })
            .count()
            >= 4
    );
}

fn assert_no_fallback_routes(candidate: &CanonicalCandidate) {
    assert!(
        validated_config(candidate)
            .http_services
            .iter()
            .all(|service| {
                service
                    .routes
                    .iter()
                    .all(|route| matches!(route.action, HttpRouteAction::Proxy { .. }))
            })
    );
}
