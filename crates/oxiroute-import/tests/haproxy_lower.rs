use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use oxiroute_config::{
    HttpHostSelector, HttpPathSelector, HttpRouteAction, ListenerBind, Protocol, UpstreamAlgorithm,
    UpstreamEndpoint, render_lua, validate_config,
};
use oxiroute_import::{
    Diagnostic, DiagnosticStage, E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, Report, Severity,
    SourceFile, SourceId,
    haproxy::{
        CanonicalCandidate, E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION,
        E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED, E_STATS_UNSUPPORTED, E_UNCONSUMED_DIRECTIVE,
        E_UNKNOWN_DIRECTIVE, E_UNSUPPORTED_FORM, HaproxyImportOptions,
        HaproxyOneRequestPerConnectionOverlay, HaproxyPrometheusMigrationOverlay, LoadedSource,
        PreprocessingEnvironment, analyze_sources, import_roots_with_environment,
        import_roots_with_options, import_sources,
    },
};
use tempfile::tempdir;

const HOSTROUTER: &[u8] = include_bytes!("fixtures/haproxy/hostrouter-active.cfg");
const SYNTHETIC_UNIX_DNS_LEASTCONN: &[u8] =
    include_bytes!("fixtures/haproxy/synthetic-unix-dns-leastconn.cfg");
const PHOENIX: &[u8] = include_bytes!("fixtures/haproxy/phoenix-dormant.cfg");
const MINIMAL: &[u8] = include_bytes!("fixtures/haproxy/minimal-representable.cfg");

#[test]
fn imported_http_services_disable_automatic_response_headers_in_canonical_rendering() {
    let lowered = import_fixture("path-routing.cfg", routing_fixture().as_bytes());
    let config = lowered
        .value()
        .config
        .as_ref()
        .expect("HAProxy HTTP config");

    assert!(
        config
            .http_services
            .iter()
            .all(|service| !service.automatic_response_headers)
    );
    let source = render_lua(config).expect("rendered HAProxy import");
    assert!(source.contains("automatic_response_headers = false,"));
    assert!(!source.contains("automatic_response_headers = true,"));
}

#[test]
fn hostrouter_active_report_finalizes_proxy_while_retaining_stats_requirements() {
    let lowered = import_fixture("hostrouter-active.cfg", HOSTROUTER);
    let candidate = lowered.value();

    assert!(candidate.config.is_some());
    assert_eq!(candidate.draft.upstream_pools.len(), 1);
    assert_eq!(
        candidate.draft.upstream_pools[0].connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Safe
    );
    assert!(
        candidate
            .draft
            .listeners
            .iter()
            .any(|listener| listener.name == "hostrouter")
    );
    assert_eq!(code_count(lowered.diagnostics(), E_LOGGING_UNSUPPORTED), 3);
    assert_eq!(code_count(lowered.diagnostics(), E_STATS_UNSUPPORTED), 2);
    assert!(candidate.draft.stats.is_none());
    assert_eq!(candidate.activation_requirements.len(), 2);
    assert_eq!(code_count(lowered.diagnostics(), E_PROCESS_OWNED), 4);
    assert_process_settings_are_external_warnings(lowered.diagnostics());
    assert_eq!(candidate.draft.max_connections, Some(4096));
    assert_has_provenance(candidate, "/max_connections");
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "forwardfor header insertion"
    ));
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "Unix bind sockets"
    ));
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "DNS-named servers"
    ));
    assert_eq!(candidate.draft.http_services.len(), 1);
    assert_no_fallback_routes(candidate);
}

#[test]
fn phoenix_dormant_report_finalizes_with_reusable_dns_leastconn_pool() {
    let lowered = import_fixture("phoenix-dormant.cfg", PHOENIX);

    assert!(lowered.value().config.is_some());
    assert_eq!(lowered.value().draft.upstream_pools.len(), 1);
    assert_eq!(
        lowered.value().draft.upstream_pools[0].connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Safe
    );
    assert_eq!(lowered.value().draft.listeners.len(), 1);
    assert_eq!(code_count(lowered.diagnostics(), E_PROCESS_OWNED), 4);
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "Unix bind sockets"
    ));
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "DNS-named servers"
    ));
    assert_no_fallback_routes(lowered.value());
}

#[test]
fn sanitized_whitebeast_haproxy_imports_from_the_complete_capture() {
    assert_complete_live_haproxy("whitebeast", Ipv4Addr::new(10, 0, 0, 10), true);
}

#[test]
fn sanitized_hostrouter_haproxy_imports_from_the_complete_capture() {
    assert_complete_live_haproxy("hostrouter", Ipv4Addr::new(10, 0, 0, 1), false);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one central live-fixture assertion pins the complete finalized policy"
)]
fn unmodified_live_hostrouter_finalizes_with_exact_compatibility_policy() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/live/hostrouter/haproxy.cfg");
    let report = import_roots_with_environment(
        &[path],
        PreprocessingEnvironment {
            node_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            gpu1_defined: false,
        },
    );
    let candidate = report.value();
    let config = candidate.config.as_ref().unwrap_or_else(|| {
        panic!(
            "live hostrouter config did not finalize: {:#?}",
            report.diagnostics()
        )
    });

    assert!(!report.has_errors(), "{:#?}", report.diagnostics());
    assert_eq!(
        code_count(report.diagnostics(), E_SEMANTICS_NOT_REPRESENTABLE),
        0
    );
    assert_eq!(code_count(report.diagnostics(), E_UNKNOWN_DIRECTIVE), 0);
    assert_eq!(code_count(report.diagnostics(), E_UNCONSUMED_DIRECTIVE), 0);
    assert_eq!(code_count(report.diagnostics(), E_LOGGING_UNSUPPORTED), 7);
    assert_eq!(code_count(report.diagnostics(), E_PROCESS_OWNED), 3);
    assert!(
        report
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.severity() == Severity::Warning)
    );
    assert!(candidate.activation_requirements.is_empty());
    let stats = config.stats.as_ref().expect("HAProxy stats page");
    assert_eq!(stats.pages.len(), 1);
    assert_eq!(stats.pages[0].bind, "0.0.0.0:8404".parse().unwrap());
    assert_eq!(stats.pages[0].uri_prefix, "/stats");
    assert_eq!(stats.pages[0].refresh_ms, 10_000);
    assert_eq!(
        stats.pages[0].admin,
        oxiroute_config::StatsPageAdminPolicy::Localhost
    );
    assert_eq!(stats.pages[0].max_connections, None);
    assert_eq!(
        stats.pages[0].downstream_timeouts.client_timeout_ms,
        Some(600_000)
    );
    assert_eq!(
        stats.pages[0].downstream_timeouts.request_timeout_ms,
        Some(600_000)
    );
    assert_eq!(
        stats.pages[0].downstream_timeouts.keepalive_timeout_ms,
        Some(60_000)
    );

    assert_eq!(config.listeners.len(), 1);
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/var/run/haproxy.sock".into(),
            mode: Some(0o777),
        }
    );
    assert_eq!(
        config.listeners[0].downstream_timeouts.client_timeout_ms,
        Some(600_000)
    );
    assert_eq!(
        config.listeners[0].downstream_timeouts.request_timeout_ms,
        Some(600_000)
    );
    assert_eq!(
        config.listeners[0].downstream_timeouts.keepalive_timeout_ms,
        Some(60_000)
    );
    let service = &config.http_services[0];
    assert_eq!(service.routes.len(), 2);
    assert!(matches!(
        service.routes[0].host,
        Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority { ref value })
            if value == "ollama.yellowmaverick.com"
    ));
    assert!(matches!(
        service.routes[1].action,
        HttpRouteAction::FixedResponse { status: 503, .. }
    ));

    let pool = &config.upstream_pools[0];
    assert_eq!(pool.algorithm, UpstreamAlgorithm::LeastConnections);
    assert_eq!(
        pool.connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Safe
    );
    let health = pool.health_check.as_ref().expect("HTTP health check");
    assert_eq!(health.interval_ms, 10_000);
    assert_eq!(health.timeout_ms, 10_000);
    assert_eq!(health.healthy_threshold, 2);
    assert_eq!(health.unhealthy_threshold, 3);
    assert_hostrouter_endpoints(pool);
    let HttpRouteAction::Proxy { policy, .. } = &service.routes[0].action else {
        panic!("host route must proxy")
    };
    assert_eq!(policy.retry.max_retries, 3);
    assert_eq!(
        policy.retry.target,
        oxiroute_config::HttpRetryTarget::SameServer
    );
    assert_eq!(policy.retry.delay_ms, 1_000);
    assert!(policy.retry.final_redispatch);
    assert_eq!(
        policy.retry.triggers,
        [
            oxiroute_config::HttpRetryTrigger::ConnectFailure,
            oxiroute_config::HttpRetryTrigger::ConnectTimeout,
        ]
    );
}

#[test]
fn sanitized_phoenix_haproxy_imports_with_explicit_environment() {
    assert_complete_live_haproxy("phoenix", Ipv4Addr::new(10, 0, 0, 11), false);
}

#[test]
fn live_inference_node_health_routes_and_native_default_retries_finalize() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/live/phoenix/haproxy.cfg");
    let report = import_roots_with_environment(
        &[path],
        PreprocessingEnvironment {
            node_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 15)),
            gpu1_defined: true,
        },
    );
    let config = report.value().config.as_ref().unwrap_or_else(|| {
        panic!(
            "inference-node config did not finalize: {:#?}",
            report.diagnostics()
        )
    });

    assert_eq!(config.http_services.len(), 3);
    assert_eq!(config.upstream_pools.len(), 3);
    assert!(
        config
            .upstream_pools
            .iter()
            .all(|pool| pool.servers.len() == 2)
    );
    for service in &config.http_services {
        assert_eq!(service.routes.len(), 2);
        assert!(matches!(
            service.routes[0].path,
            HttpPathSelector::AsciiCaseInsensitiveExact { ref value }
                if value == "/_infra/health"
        ));
        assert!(matches!(
            service.routes[0].action,
            HttpRouteAction::FixedResponse { status: 200, ref body, .. } if body == "ok"
        ));
        let HttpRouteAction::Proxy { ref policy, .. } = service.routes[1].action else {
            panic!("inference-node fallback route")
        };
        assert_eq!(policy.retry.max_retries, 3);
    }
}

fn assert_complete_live_haproxy(host: &str, node_ip: Ipv4Addr, gpu1_defined: bool) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/live")
        .join(host)
        .join("haproxy.cfg");
    let options = HaproxyImportOptions::default();
    let report = import_roots_with_options(
        &[path],
        PreprocessingEnvironment {
            node_ip: IpAddr::V4(node_ip),
            gpu1_defined,
        },
        &options,
    );
    assert_eq!(code_count(report.diagnostics(), E_UNKNOWN_DIRECTIVE), 0);
    assert_eq!(code_count(report.diagnostics(), E_UNCONSUMED_DIRECTIVE), 0);
    assert_eq!(code_count(report.diagnostics(), E_ENVIRONMENT_EXPANSION), 0);
    assert_eq!(
        code_count(report.diagnostics(), E_CONDITIONAL_PREPROCESSING),
        0
    );
    if host == "hostrouter" {
        assert_eq!(report.value().draft.upstream_pools.len(), 1);
        assert_eq!(
            report.value().draft.upstream_pools[0].connection_reuse,
            oxiroute_config::UpstreamConnectionReuse::Safe
        );
        assert!(!diagnostic_contains(
            report.diagnostics(),
            "requires option http-server-close"
        ));
        assert!(report.value().operational_overlays.is_empty());
    } else {
        assert!(!report.value().draft.upstream_pools.is_empty());
    }
    let queue_timeouts = report
        .value()
        .draft
        .upstream_pools
        .iter()
        .map(|pool| pool.queue_timeout_ms)
        .collect::<Vec<_>>();
    match host {
        "whitebeast" => {
            assert!(queue_timeouts.contains(&Some(1_800_000)));
            assert_eq!(report.value().draft.max_connections, Some(1_024));
        }
        "phoenix" => assert!(
            queue_timeouts
                .iter()
                .all(|timeout| *timeout == Some(600_000))
        ),
        "hostrouter" => assert!(queue_timeouts.iter().all(Option::is_none)),
        _ => unreachable!(),
    }
    assert!(
        report
            .value()
            .source_metadata
            .environment_fingerprint_sha256
            .is_some()
    );
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
    let config = candidate.config.as_ref().expect("audited lifecycle config");
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
    assert!(wrong_backend.value().config.is_none());
    assert!(!wrong_backend.value().operational_overlays[0].satisfied);
}

#[test]
fn unix_at_listener_mode_lowers_with_field_provenance() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout server 30s
frontend public
  bind unix@/run/haproxy/public.sock mode 777
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("unix-mode.cfg", source);
    let candidate = lowered.value();
    let config = candidate.config.as_ref().expect("Unix mode config");

    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/run/haproxy/public.sock".into(),
            mode: Some(0o777),
        }
    );
    assert_has_provenance(candidate, "/listeners/0/bind/path");
    assert_has_provenance(candidate, "/listeners/0/bind/mode");
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
        .config
        .as_ref()
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
        .config
        .as_ref()
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

    assert!(imported.value().config.is_none());
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

        assert!(imported.value().config.is_none(), "{name} was truncated");
        assert!(
            diagnostic_contains(imported.diagnostics(), "exactly representable"),
            "{name}: {:?}",
            imported.diagnostics()
        );
    }
}

#[test]
fn minimal_static_tcp_fixture_finalizes_and_validates() {
    let lowered = import_fixture("minimal-representable.cfg", MINIMAL);

    assert!(lowered.diagnostics().is_empty());
    let candidate = lowered.value();
    let config = candidate.config.as_ref().expect("finalized config");
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.upstream_pools.len(), 1);
    assert_eq!(config.l4_services.len(), 1);
    assert!(config.http_services.is_empty());
    assert_eq!(config.listeners[0].name, "postgres");
    assert_eq!(config.listeners[0].protocol, Protocol::Tcp);
    assert_eq!(config.listeners[0].max_connections, Some(1000));
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Socket {
            address: "127.0.0.1:15432".parse::<SocketAddr>().unwrap()
        }
    );
    assert_eq!(config.upstream_pools[0].name, "postgres_pool");
    assert_eq!(config.upstream_pools[0].queue_timeout_ms, Some(15_000));
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::RoundRobin
    );
    assert_eq!(
        config.upstream_pools[0]
            .servers
            .iter()
            .map(|server| server.endpoint.clone())
            .collect::<Vec<_>>(),
        [UpstreamEndpoint::Socket {
            address: "127.0.0.1:5432".parse::<SocketAddr>().unwrap()
        }]
    );
    assert_eq!(config.l4_services[0].connect_timeout_ms, 10_000);
    assert_eq!(config.l4_services[0].idle_timeout_ms, 300_000);
    assert_eq!(config.l4_services[0].upstream_pool, "postgres_pool");
    let mut independently_validated = config.clone();
    validate_config(&mut independently_validated).expect("canonical validation");
    assert_eq!(&independently_validated, config);
    assert!(
        candidate
            .provenance
            .iter()
            .any(|provenance| provenance.path == "/listeners/0")
    );
    assert!(candidate.provenance.iter().any(|provenance| {
        provenance.path == "/l4_services/0"
            && provenance
                .origins
                .iter()
                .any(|origin| origin.role == oxiroute_import::ProvenanceRole::Inherited)
    }));
    assert!(candidate.provenance.iter().any(|provenance| {
        provenance.path == "/listeners/0/service"
            && provenance
                .origins
                .iter()
                .any(|origin| origin.role == oxiroute_import::ProvenanceRole::Declaration)
    }));
    assert!(candidate.provenance.iter().any(|provenance| {
        provenance.path == "/l4_services/0/upstream_pool"
            && provenance
                .origins
                .iter()
                .any(|origin| origin.role == oxiroute_import::ProvenanceRole::Reference)
    }));
    assert!(candidate.provenance.iter().any(|provenance| {
        provenance.path == "/listeners/0/protocol"
            && provenance
                .origins
                .iter()
                .any(|origin| origin.role == oxiroute_import::ProvenanceRole::Inherited)
    }));
    assert!(candidate.provenance.iter().any(|provenance| {
        provenance.path == "/listeners/0/max_connections"
            && provenance
                .origins
                .iter()
                .any(|origin| origin.role == oxiroute_import::ProvenanceRole::Value)
    }));
    for path in [
        "/listeners/0/bind/type",
        "/listeners/0/bind/address",
        "/upstream_pools/0/servers/0/endpoint/type",
        "/upstream_pools/0/servers/0/endpoint/address",
        "/upstream_pools/0/algorithm",
    ] {
        assert_has_provenance(candidate, path);
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
        .config
        .as_ref()
        .expect("finalized hostrouter subset");
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/run/haproxy/hostrouter.sock".into(),
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
fn bind_only_and_explicitly_unbounded_frontend_limits_lower_without_guessing_a_cap() {
    let bind_only = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace("bind 127.0.0.1:15432", "bind 127.0.0.1:15432 maxconn 75")
        .replace("  maxconn 1000\n", "");
    let unbounded = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace("maxconn 1000", "maxconn 0");

    let bind_only = import_fixture("bind-only-cap.cfg", bind_only.as_bytes());
    assert_eq!(
        bind_only
            .value()
            .config
            .as_ref()
            .expect("bind-only cap")
            .listeners[0]
            .max_connections,
        Some(75)
    );
    assert_has_provenance(bind_only.value(), "/listeners/0/max_connections");

    let unbounded = import_fixture("explicit-unbounded.cfg", unbounded.as_bytes());
    assert_eq!(
        unbounded
            .value()
            .config
            .as_ref()
            .expect("explicit frontend fallback to process admission")
            .listeners[0]
            .max_connections,
        None
    );
    assert_has_provenance(unbounded.value(), "/listeners/0/max_connections");
}

#[test]
fn incomplete_tcp_timeout_policy_emits_no_disconnected_listener_or_service() {
    let source = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace("  timeout connect 10s\n", "");
    let lowered = import_fixture("missing-connect-timeout.cfg", source.as_bytes());

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.listeners.is_empty());
    assert!(lowered.value().draft.l4_services.is_empty());
    assert_eq!(lowered.value().draft.upstream_pools.len(), 1);
    assert_blocker(lowered.diagnostics(), "timeout connect must be explicit");
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
        .config
        .as_ref()
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
fn representable_tcp_listen_lowers_its_implicit_backend_reference() {
    let source = b"defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
listen database
  bind 127.0.0.1:25432
  maxconn 250
  balance roundrobin
  server primary 127.0.0.1:5432
";
    let lowered = import_fixture("listen.cfg", source);
    let config = lowered.value().config.as_ref().expect("finalized listen");

    assert!(lowered.diagnostics().is_empty());
    assert_eq!(config.listeners[0].service.as_deref(), Some("database"));
    assert_eq!(config.upstream_pools[0].name, "database");
    assert_eq!(config.l4_services[0].upstream_pool, "database");
}

#[test]
fn explicit_frontend_and_backend_modes_reach_protocol_and_service_provenance() {
    let source = b"defaults tcp_defaults
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend database
  mode tcp
  bind 127.0.0.1:35432
  maxconn 250
  default_backend database_pool
backend database_pool
  mode tcp
  balance roundrobin
  server primary 127.0.0.1:5432
";
    let lowered = import_fixture("explicit-modes.cfg", source);
    let candidate = lowered.value();

    assert!(candidate.config.is_some());
    for path in ["/listeners/0/protocol", "/l4_services/0"] {
        let provenance = candidate
            .provenance
            .iter()
            .find(|provenance| provenance.path == path)
            .unwrap_or_else(|| panic!("missing {path} provenance"));
        assert!(
            provenance
                .origins
                .iter()
                .filter(|origin| origin.role == oxiroute_import::ProvenanceRole::Value)
                .count()
                >= 2,
            "{path} must retain frontend and backend mode values"
        );
    }
}

#[test]
fn raw_path_prefix_acl_is_not_widened_or_narrowed_to_segment_matching() {
    let lowered = import_fixture("path-routing.cfg", routing_fixture().as_bytes());

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let config = lowered.value().config.as_ref().expect("raw-prefix config");
    assert!(matches!(
        config.http_services[0].routes[0].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/api"
    ));
}

#[test]
fn host_header_acl_is_blocked_because_canonical_host_matching_normalizes_ports() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:8080
  maxconn 100
  acl app_host hdr(host) app.example
  use_backend app if app_host
  default_backend fallback
backend app
  balance roundrobin
  server app1 127.0.0.1:3001
backend fallback
  balance roundrobin
  server fallback1 127.0.0.1:3002
";
    let lowered = import_fixture("host-routing.cfg", source);

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let config = lowered.value().config.as_ref().expect("authority config");
    assert!(matches!(
        config.http_services[0].routes[0].host,
        Some(HttpHostSelector::ExactAuthority { ref value }) if value == "app.example"
    ));
}

#[test]
fn case_insensitive_host_acl_lowers_to_exact_authority_without_port_widening() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:8080
  maxconn 100
  acl app_host hdr(host) -i app.example
  use_backend app if app_host
backend app
  balance roundrobin
  server app1 127.0.0.1:3001
";
    let lowered = import_fixture("case-insensitive-host.cfg", source);

    let config = lowered.value().config.as_ref().unwrap_or_else(|| {
        panic!(
            "case-insensitive authority did not finalize: {:#?}",
            lowered.diagnostics()
        )
    });
    assert!(matches!(
        config.http_services[0].routes[0].host,
        Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority { ref value })
            if value == "app.example"
    ));
    assert!(matches!(
        config.http_services[0].routes[1].action,
        HttpRouteAction::FixedResponse { status: 503, .. }
    ));
}

#[test]
fn conditional_backend_without_default_appends_a_last_catch_all_503() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:8080
  acl app_host hdr(host) app.example
  use_backend app if app_host
backend app
  balance roundrobin
  server app1 127.0.0.1:3001
";
    let lowered = import_fixture("no-default-backend.cfg", source);
    let config = lowered
        .value()
        .config
        .as_ref()
        .expect("503 fallback config");
    let routes = &config.http_services[0].routes;

    assert_eq!(routes.len(), 2);
    assert!(matches!(routes[0].action, HttpRouteAction::Proxy { .. }));
    assert!(routes[1].host.is_none());
    assert!(matches!(
        routes[1].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/"
    ));
    assert!(matches!(
        routes[1].action,
        HttpRouteAction::FixedResponse { status: 503, .. }
    ));
}

#[test]
fn preprocessing_unknown_and_unsupported_semantics_flow_into_the_candidate_report() {
    let source = b".if defined(ENABLED)
defaults web
  mode http
  mystery value
  option magical
frontend public
  bind \"${BIND-127.0.0.1:8080}\"
  .endif
";
    let path = PathBuf::from("preprocessing.cfg");
    let lowered = import_sources(&[LoadedSource {
        root_ordinal: 0,
        file_ordinal: 0,
        source: SourceFile::from_path(SourceId::new(0), path.clone(), source.as_slice()),
        path,
    }]);

    assert!(lowered.value().config.is_none());
    for code in [
        E_CONDITIONAL_PREPROCESSING,
        E_ENVIRONMENT_EXPANSION,
        E_UNKNOWN_DIRECTIVE,
        E_UNSUPPORTED_FORM,
    ] {
        assert!(lowered.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == code && diagnostic.primary_span().is_some()
        }));
    }
}

#[test]
fn unsupported_inherited_mode_emits_no_http_listener_or_service() {
    let source = b"defaults shared
  mode health
  retries 0
  timeout connect 30s
  timeout client 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:8080
  maxconn 100
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("unsupported-mode.cfg", source);

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.listeners.is_empty());
    assert!(lowered.value().draft.http_services.is_empty());
    assert_blocker(lowered.diagnostics(), "unsupported HAProxy mode");
}

#[test]
fn canonical_validation_failure_is_a_blocking_validate_diagnostic() {
    let source = b"defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend first
  bind 127.0.0.1:15432
  maxconn 100
  default_backend pool
frontend second
  bind 127.0.0.1:15432
  maxconn 100
  default_backend pool
backend pool
  balance roundrobin
  server primary 127.0.0.1:5432
";
    let lowered = import_fixture("overlapping-binds.cfg", source);

    assert!(lowered.value().config.is_none());
    assert!(lowered.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_INVALID_VALUE
            && diagnostic.stage() == DiagnosticStage::Validate
            && diagnostic.message().contains("overlap")
    }));
}

#[test]
fn tcp_to_http_backend_mode_transition_emits_no_listener_or_service() {
    let source = b"frontend public
  mode tcp
  timeout client 30s
  bind 127.0.0.1:15432
  maxconn 100
  default_backend app
backend app
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("tcp-to-http.cfg", source);

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.listeners.is_empty());
    assert!(lowered.value().draft.l4_services.is_empty());
    assert_blocker(
        lowered.diagnostics(),
        "HAProxy frontend TCP mode transitions to an HTTP backend",
    );
}

#[test]
fn listen_to_explicit_backend_mode_transition_is_blocking() {
    let source = b"listen database
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
  bind 127.0.0.1:25432
  maxconn 250
  balance roundrobin
  server local 127.0.0.1:5432
  default_backend web
backend web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
  balance roundrobin
  server web1 127.0.0.1:3000
";
    let lowered = import_fixture("listen-transition.cfg", source);

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.listeners.is_empty());
    assert!(lowered.value().draft.l4_services.is_empty());
    assert_blocker(
        lowered.diagnostics(),
        "HAProxy listen TCP mode transitions to an HTTP backend",
    );
}

#[test]
fn automatic_or_aggregate_maxconn_never_emits_an_optional_cap_placeholder() {
    let missing = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace("  maxconn 1000\n", "");
    let aggregate = String::from_utf8(MINIMAL.to_vec())
        .expect("UTF-8 fixture")
        .replace(
            "  bind 127.0.0.1:15432\n",
            "  bind 127.0.0.1:15432,127.0.0.1:15433\n",
        );

    let missing = import_fixture("unbounded-admission.cfg", missing.as_bytes());
    let config = missing
        .value()
        .config
        .as_ref()
        .expect("frontend without a local cap");
    assert_eq!(config.listeners[0].max_connections, None);
    assert!(
        missing
            .value()
            .provenance
            .iter()
            .all(|provenance| { provenance.path != "/listeners/0/max_connections" })
    );

    let aggregate = import_fixture("aggregate-admission.cfg", aggregate.as_bytes());
    assert!(aggregate.value().config.is_none());
    assert!(aggregate.value().draft.listeners.is_empty());
    assert_blocker(
        aggregate.diagnostics(),
        "proxy maxconn is aggregate across binds",
    );
}

#[test]
fn global_and_bind_maxconn_lower_to_their_distinct_canonical_scopes() {
    let global_fallback = b"global
  maxconn 500
defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend database
  bind 127.0.0.1:15432 maxconn 100
  maxconn 100
  default_backend database_pool
backend database_pool
  balance roundrobin
  server primary 127.0.0.1:5432
";
    let bind_cap = b"defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend database
  bind 127.0.0.1:15432 maxconn 75
  maxconn 100
  default_backend database_pool
backend database_pool
  balance roundrobin
  server primary 127.0.0.1:5432
";

    let global_fallback = import_fixture("global-admission.cfg", global_fallback);
    let config = global_fallback
        .value()
        .config
        .as_ref()
        .expect("global admission limit");
    assert!(!config.listeners.is_empty(), "{global_fallback:#?}");
    assert_eq!(config.max_connections, Some(500));
    assert_eq!(config.listeners[0].max_connections, Some(100));
    assert_has_provenance(global_fallback.value(), "/max_connections");

    let bind_cap = import_fixture("bind-admission.cfg", bind_cap);
    let config = bind_cap
        .value()
        .config
        .as_ref()
        .expect("exact bind admission");
    assert_eq!(config.listeners[0].max_connections, Some(75));
    assert_has_provenance(bind_cap.value(), "/listeners/0/max_connections");
}

#[test]
fn explicit_preprocessing_records_environment_and_inactive_gpu_provenance() {
    let directory = tempdir().expect("preprocessing fixture directory");
    let root = directory.path().join("haproxy.cfg");
    fs::write(
        &root,
        b"global\n  maxconn 64\n  log stdout format raw local0\n  user haproxy\ndefaults\n  mode tcp\n  retries 0\n  timeout connect 5s\n  timeout client 2h\n  timeout server 2h\nfrontend node\n  bind ${NODE_IP}:10440 maxconn 10\n  maxconn 10\n  default_backend workers\nbackend workers\n  balance first\n  option http-server-close\n  server gpu0 127.0.0.1:10450 maxconn 1 no-check\n.if defined(GPU1)\n  server gpu1 127.0.0.1:10451 maxconn 1 no-check\n.endif\n",
    )
    .expect("write preprocessing fixture");

    let without_gpu = import_roots_with_environment(
        &[&root],
        PreprocessingEnvironment {
            node_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            gpu1_defined: false,
        },
    );
    let candidate = without_gpu.value();
    let config = candidate.config.as_ref().expect("preprocessed config");
    assert!(!config.listeners.is_empty(), "{without_gpu:#?}");
    assert_eq!(config.max_connections, Some(64));
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Socket {
            address: "192.0.2.10:10440".parse().unwrap()
        }
    );
    assert_eq!(config.upstream_pools[0].servers.len(), 1);
    assert_eq!(config.upstream_pools[0].algorithm, UpstreamAlgorithm::First);
    assert_eq!(config.upstream_pools[0].servers[0].max_connections, Some(1));
    assert_eq!(
        config.upstream_pools[0].connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Never
    );
    assert_eq!(candidate.deployment_requirements.len(), 2);
    assert_eq!(candidate.source_metadata.inactive_sources.len(), 1);
    assert_eq!(candidate.source_metadata.original_sources.len(), 1);
    assert_eq!(candidate.source_metadata.source_maps.len(), 1);
    assert_eq!(
        candidate.source_metadata.original_sources[0].bytes(),
        fs::read(&root).unwrap()
    );
    let source_map = &candidate.source_metadata.source_maps[0];
    let original = &candidate.source_metadata.original_sources[0];
    assert!(candidate.provenance.iter().any(|entry| {
        entry.origins.iter().any(|origin| {
            source_map
                .translate(origin.span)
                .and_then(|span| original.slice(span.range()))
                .is_some_and(|bytes| {
                    bytes
                        .windows(b"${NODE_IP}".len())
                        .any(|part| part == b"${NODE_IP}")
                })
        })
    }));
    assert_eq!(
        candidate.source_metadata.inactive_sources[0].condition,
        "defined(GPU1)"
    );
    let without_gpu_fingerprint = candidate
        .source_metadata
        .environment_fingerprint_sha256
        .as_deref()
        .expect("environment fingerprint");
    assert_eq!(without_gpu_fingerprint.len(), 64);

    let with_gpu = import_roots_with_environment(
        &[&root],
        PreprocessingEnvironment {
            node_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            gpu1_defined: true,
        },
    );
    let candidate = with_gpu.value();
    assert_eq!(
        candidate
            .config
            .as_ref()
            .expect("GPU config")
            .upstream_pools[0]
            .servers
            .len(),
        2
    );
    assert!(candidate.source_metadata.inactive_sources.is_empty());
    assert_ne!(
        candidate
            .source_metadata
            .environment_fingerprint_sha256
            .as_deref()
            .expect("GPU environment fingerprint"),
        without_gpu_fingerprint
    );
}

#[test]
fn prometheus_service_retains_migration_requirement_without_an_operator_overlay() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout server 30s
frontend metrics
  bind 127.0.0.1:8404
  http-request use-service prometheus-exporter if { path /metrics }
frontend app
  bind 127.0.0.1:8080 maxconn 10
  maxconn 10
  default_backend workers
backend workers
  balance roundrobin
  server app1 127.0.0.1:3000
";

    let imported = import_fixture("prometheus-activation.cfg", source);
    let candidate = imported.value();
    let config = candidate.config.as_ref().expect("unrelated app config");
    assert_eq!(config.listeners.len(), 1);
    assert!(config.stats.is_none());
    assert_eq!(candidate.activation_requirements.len(), 1);
    assert_eq!(
        candidate.activation_requirements[0].kind,
        oxiroute_import::ActivationRequirementKind::PrometheusExporter
    );
    assert!(!candidate.activation_requirements[0].equivalent_runtime_endpoint);
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 1);
}

#[test]
fn dedicated_supported_stats_sections_lower_only_to_canonical_pages() {
    let source = b"frontend stats
  mode http
  bind *:8404
  maxconn 300
  timeout client 30s
  timeout http-request 5s
  timeout http-keep-alive 2s
  stats uri /stats
  stats refresh 10s
  stats admin if LOCALHOST
listen public-stats
  mode http
  bind 127.0.0.1:8405
  stats enable
  stats uri /public
  stats refresh 5s
";

    let imported = import_fixture("stats-pages.cfg", source);
    assert!(!imported.has_errors(), "{:?}", imported.diagnostics());
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 0);
    let candidate = imported.value();
    let config = candidate.config.as_ref().expect("stats pages config");
    let stats = config.stats.as_ref().expect("canonical stats");
    assert!(stats.binds.is_empty());
    assert_eq!(stats.pages.len(), 2);
    assert_eq!(stats.pages[0].bind, "0.0.0.0:8404".parse().unwrap());
    assert_eq!(stats.pages[0].uri_prefix, "/stats");
    assert_eq!(stats.pages[0].refresh_ms, 10_000);
    assert_eq!(stats.pages[0].max_connections, Some(300));
    assert_eq!(
        stats.pages[0].downstream_timeouts.client_timeout_ms,
        Some(30_000)
    );
    assert_eq!(
        stats.pages[0].downstream_timeouts.request_timeout_ms,
        Some(5_000)
    );
    assert_eq!(
        stats.pages[0].downstream_timeouts.keepalive_timeout_ms,
        Some(2_000)
    );
    assert_eq!(
        stats.pages[0].admin,
        oxiroute_config::StatsPageAdminPolicy::Localhost
    );
    assert_eq!(
        stats.pages[1].admin,
        oxiroute_config::StatsPageAdminPolicy::Disabled
    );
    assert!(config.listeners.is_empty());
    assert!(config.http_services.is_empty());
    assert!(config.l4_services.is_empty());
    assert!(candidate.activation_requirements.is_empty());
    for path in [
        "/stats/pages/0",
        "/stats/pages/0/bind",
        "/stats/pages/0/uri_prefix",
        "/stats/pages/0/refresh_ms",
        "/stats/pages/0/admin",
        "/stats/pages/0/max_connections",
        "/stats/pages/0/downstream_timeouts/client_timeout_ms",
        "/stats/pages/0/downstream_timeouts/request_timeout_ms",
        "/stats/pages/0/downstream_timeouts/keepalive_timeout_ms",
    ] {
        assert_has_provenance(candidate, path);
    }
}

#[test]
fn stats_frontend_response_rules_fail_closed_instead_of_disappearing() {
    let source = b"frontend stats
  mode http
  bind 127.0.0.1:8404
  stats uri /stats
  stats refresh 10s
  http-response set-header x-stats-policy retained
";

    let imported = import_fixture("stats-response-policy.cfg", source);

    assert!(imported.value().config.is_none());
    assert!(imported.value().draft.stats.is_none());
    assert!(diagnostic_contains(
        imported.diagnostics(),
        "stats frontend response rules"
    ));
}

#[test]
fn stats_frontend_connection_close_policy_fails_closed_instead_of_disappearing() {
    let source = b"frontend stats
  mode http
  bind 127.0.0.1:8404
  option http-server-close
  stats uri /stats
  stats refresh 10s
";

    let imported = import_fixture("stats-connection-policy.cfg", source);

    assert!(imported.value().config.is_none());
    assert!(imported.value().draft.stats.is_none());
    assert!(imported.has_errors());
}

#[test]
fn unsupported_stats_form_suppresses_page_without_creating_an_ordinary_service() {
    let source = b"frontend stats
  mode http
  bind *:8404
  stats enable
  stats uri /stats
  stats refresh 10s
  stats hide-version
";

    let imported = import_fixture("blocked-stats-page.cfg", source);
    let candidate = imported.value();
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 1);
    assert_eq!(candidate.activation_requirements.len(), 1);
    assert!(candidate.draft.stats.is_none());
    assert!(candidate.draft.listeners.is_empty());
    assert!(candidate.draft.http_services.is_empty());
}

#[test]
fn stats_auth_suppresses_page_without_creating_an_ordinary_service() {
    let source = b"frontend stats
  mode http
  bind *:8404
  stats uri /stats
  stats auth operator:secret
";

    let imported = import_fixture("authenticated-stats-page.cfg", source);
    let candidate = imported.value();
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 1);
    assert!(candidate.draft.stats.is_none());
    assert!(candidate.draft.listeners.is_empty());
    assert!(candidate.draft.http_services.is_empty());
}

#[test]
fn explicit_prometheus_migration_overlay_enables_the_distinct_oxiroute_stats_contract() {
    let source = b"frontend metrics
  bind 127.0.0.1:8404
  stats auth operator:secret
  stats uri /admin
  stats admin if TRUE
  http-request use-service prometheus-exporter if { path /metrics }
";

    let parsed = analyze_fixture("strict-stats-forms.cfg", source);
    let imported = import_fixture_with_options(
        "strict-stats-forms.cfg",
        source,
        &HaproxyImportOptions {
            one_request_per_connection: Vec::new(),
            prometheus_migrations: vec![HaproxyPrometheusMigrationOverlay {
                section: "metrics".into(),
            }],
        },
    );
    assert_eq!(
        parsed.value().supported_stats_sections.len(),
        1,
        "fixture must remain an exact native Prometheus service"
    );
    let candidate = imported.value();
    assert_eq!(
        candidate
            .draft
            .stats
            .as_ref()
            .expect("exact Prometheus endpoint")
            .binds,
        ["127.0.0.1:8404".parse().unwrap()]
    );
    assert_eq!(candidate.activation_requirements.len(), 3);
    assert!(
        candidate
            .activation_requirements
            .iter()
            .all(|requirement| { !requirement.equivalent_runtime_endpoint })
    );
    assert!(candidate.operational_overlays.iter().any(|overlay| {
        overlay.kind == oxiroute_import::OperationalOverlayKind::PrometheusMigration
            && overlay.satisfied
    }));
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 3);
}

#[test]
fn prometheus_migration_overlay_must_uniquely_match_a_dedicated_exact_service() {
    let source = b"frontend metrics
  bind 127.0.0.1:8404
  http-request use-service prometheus-exporter if { path /metrics }
";
    for sections in [vec!["missing"], vec!["metrics", "metrics"]] {
        let imported = import_fixture_with_options(
            "invalid-prometheus-migration.cfg",
            source,
            &HaproxyImportOptions {
                one_request_per_connection: Vec::new(),
                prometheus_migrations: sections
                    .into_iter()
                    .map(|section| HaproxyPrometheusMigrationOverlay {
                        section: section.into(),
                    })
                    .collect(),
            },
        );
        assert!(imported.value().config.is_none());
        assert!(imported.value().draft.stats.is_none());
        assert!(imported.value().operational_overlays.iter().all(|overlay| {
            overlay.kind != oxiroute_import::OperationalOverlayKind::PrometheusMigration
                || !overlay.satisfied
        }));
        assert!(diagnostic_contains(
            imported.diagnostics(),
            "must uniquely match one exact, dedicated Prometheus service"
        ));
    }
}

#[test]
fn forwardfor_loopback_exception_lowers_to_canonical_source_cidr_policy() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout server 30s
frontend app
  bind 127.0.0.1:8080 maxconn 10
  maxconn 10
  option forwardfor except 127.0.0.0/8
  default_backend workers
backend workers
  balance roundrobin
  server app1 127.0.0.1:3000
";

    let imported = import_fixture("forwardfor-except.cfg", source);
    let config = imported.value().config.as_ref().expect("forwardfor config");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route")
    };
    assert!(policy.request_headers.iter().any(|mutation| {
        matches!(
            mutation,
            oxiroute_config::HttpRequestHeaderMutation::Set {
                name,
                value: oxiroute_config::HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 8_192,
                    except_source_cidrs,
                },
            } if name == "x-forwarded-for" && except_source_cidrs == &["127.0.0.0/8"]
        )
    }));
}

#[test]
fn inherited_proxy_cap_and_per_socket_caps_preserve_their_native_scopes() {
    let inherited = b"defaults tcp_defaults
  mode tcp
  maxconn 100
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend inherited
  bind 127.0.0.1:16432
  default_backend pool
backend pool
  balance roundrobin
  server primary 127.0.0.1:5432
";
    let per_socket = b"defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend sockets
  bind 127.0.0.1:17432,127.0.0.1:17433 maxconn 40
  maxconn 100
  default_backend pool
backend pool
  balance roundrobin
  server primary 127.0.0.1:5432
";

    let inherited = import_fixture("inherited-admission.cfg", inherited);
    let inherited_config = inherited
        .value()
        .config
        .as_ref()
        .expect("inherited admission");
    assert_eq!(inherited_config.listeners[0].max_connections, Some(100));
    let inherited_origins = &inherited
        .value()
        .provenance
        .iter()
        .find(|provenance| provenance.path == "/listeners/0/max_connections")
        .expect("inherited cap provenance")
        .origins;
    assert!(
        inherited_origins
            .iter()
            .any(|origin| origin.role == oxiroute_import::ProvenanceRole::Inherited)
    );

    let per_socket = import_fixture("per-socket-admission.cfg", per_socket);
    let per_socket_config = per_socket
        .value()
        .config
        .as_ref()
        .expect("per-socket admission");
    assert_eq!(per_socket_config.listeners.len(), 2);
    assert!(
        per_socket_config
            .listeners
            .iter()
            .all(|listener| listener.max_connections == Some(40))
    );
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
        .config
        .as_ref()
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

        assert!(lowered.value().config.is_none(), "{backend}");
        assert_blocker(lowered.diagnostics(), "server maxconn/first");
    }
}

#[test]
fn bare_redispatch_lowers_to_delayed_same_server_retries_and_final_redispatch() {
    let source = b"defaults web
  mode http
  retries 3
  option redispatch
  timeout connect 600s
  timeout server 600s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
  server app2 127.0.0.1:3001
";
    let lowered = import_fixture("redispatch.cfg", source);
    let config = lowered.value().config.as_ref().unwrap_or_else(|| {
        panic!(
            "bare redispatch did not finalize: {:#?}",
            lowered.diagnostics()
        )
    });
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route")
    };

    assert_eq!(policy.retry.max_retries, 3);
    assert_eq!(
        policy.retry.target,
        oxiroute_config::HttpRetryTarget::SameServer
    );
    assert_eq!(policy.retry.delay_ms, 1_000);
    assert!(policy.retry.final_redispatch);
    assert_eq!(
        policy.retry.triggers,
        [
            oxiroute_config::HttpRetryTrigger::ConnectFailure,
            oxiroute_config::HttpRetryTrigger::ConnectTimeout,
        ]
    );
}

#[test]
fn redispatch_interval_forms_remain_blocking() {
    let source = b"defaults web
  mode http
  retries 3
  option redispatch 2
  timeout connect 5s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("redispatch-interval.cfg", source);

    assert!(lowered.value().config.is_none());
    assert_blocker(lowered.diagnostics(), "redispatch interval forms");
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

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.upstream_pools.is_empty());
    assert_blocker(
        lowered.diagnostics(),
        "server selection, capacity, TLS, or check option",
    );
}

#[test]
fn raw_routing_subset_retains_the_explicit_unbounded_body_policy() {
    let lowered = import_fixture("http-body-policy.cfg", routing_fixture().as_bytes());

    let config = lowered.value().config.as_ref().expect("raw routing config");
    assert_eq!(config.http_services[0].max_request_body_bytes, None);
    assert_has_provenance(lowered.value(), "/http_services/0/max_request_body_bytes");
}

#[test]
fn strict_default_route_http_subset_uses_an_explicit_unbounded_body_policy() {
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
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("strict-http.cfg", source);

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let candidate = lowered.value();
    let config = candidate.config.as_ref().expect("finalized strict HTTP");
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(config.http_services[0].max_request_body_bytes, None);
    assert_eq!(config.http_services[0].routes.len(), 1);
    assert!(matches!(
        config.http_services[0].routes[0].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/"
    ));
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.max_retries, 0);
    assert_has_provenance(candidate, "/http_services/0/max_request_body_bytes");
}

#[test]
fn positive_http_retries_finalize_for_unrestricted_methods_as_pre_send_connection_retries() {
    let source = b"defaults web
  mode http
  retries 1
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  maxconn 100
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("positive-http-retries.cfg", source);

    assert!(
        lowered.value().config.is_some(),
        "{:?}",
        lowered.diagnostics()
    );
    let HttpRouteAction::Proxy { policy, .. } =
        &lowered.value().config.as_ref().unwrap().http_services[0].routes[0].action
    else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.max_retries, 1);
}

#[test]
fn unconditional_fixed_response_and_redirect_actions_finalize() {
    let fixed = b"frontend health
  mode http
  bind 127.0.0.1:18081
  maxconn 100
  http-request return status 200 content-type text/plain string healthy
";
    let redirect = b"frontend redirect
  mode http
  bind 127.0.0.1:18082
  maxconn 100
  http-request redirect location https://example.test/new code 308
";

    let fixed = import_fixture("fixed-response.cfg", fixed);
    assert!(fixed.diagnostics().is_empty(), "{:?}", fixed.diagnostics());
    assert!(matches!(
        fixed.value().config.as_ref().expect("fixed config").http_services[0].routes[0].action,
        HttpRouteAction::FixedResponse { status: 200, ref body, .. } if body == "healthy"
    ));
    assert_has_provenance(fixed.value(), "/http_services/0/routes/0/action/status");
    assert!(
        fixed
            .value()
            .provenance
            .iter()
            .all(|provenance| provenance.path != "/http_services/0/routes/0/action/upstream_pool")
    );

    let redirect = import_fixture("redirect.cfg", redirect);
    assert!(
        redirect.diagnostics().is_empty(),
        "{:?}",
        redirect.diagnostics()
    );
    assert!(matches!(
        redirect
            .value()
            .config
            .as_ref()
            .expect("redirect config")
            .http_services[0]
            .routes[0]
            .action,
        HttpRouteAction::Redirect { status: 308, .. }
    ));
}

#[test]
fn representable_forward_header_mutations_lower_into_proxy_policy() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18083
  maxconn 100
  http-request set-header X-Client-IP %[src]
  http-request del-header X-Remove
  default_backend app
backend app
  balance roundrobin
  http-response set-header X-Frame-Options same-origin
  http-response del-header X-Powered-By
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("header-mutations.cfg", source);

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let route = &lowered
        .value()
        .config
        .as_ref()
        .expect("header config")
        .http_services[0]
        .routes[0];
    let HttpRouteAction::Proxy { policy, .. } = &route.action else {
        panic!("proxy action");
    };
    assert_eq!(policy.request_headers.len(), 2);
    assert_eq!(policy.response_headers.len(), 2);
    assert_has_provenance(
        lowered.value(),
        "/http_services/0/routes/0/action/policy/request_headers",
    );
}

#[test]
fn public_source_import_carries_syntax_diagnostics_through_finalization() {
    let lowered = import_fixture("syntax.cfg", b"frontend public\n  bind 127.0.0.1:8080");

    assert!(lowered.value().config.is_none());
    assert!(
        lowered
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == oxiroute_import::haproxy::E_SYNTAX)
    );
}

#[test]
fn tls_bind_retains_pem_san_identities_sidecar_key_and_downstream_timeout() {
    let certificate_path = fixture_path("tls-chain.pem");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout client 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display()
    );
    let resolved = analyze_fixture("tls.cfg", source.as_bytes());
    let tls = resolved.value().frontends[0].binds[0]
        .tls
        .as_ref()
        .expect("resolved TLS bind");
    assert_eq!(tls.value.dns_names, ["proxy.example.test"]);
    assert_eq!(tls.value.certificate_chain_path, certificate_path);
    assert_eq!(
        tls.value.private_key_path,
        certificate_path.with_file_name("tls-chain.pem.key")
    );

    let lowered = import_fixture("tls.cfg", source.as_bytes());
    let candidate = lowered.value();

    let config = candidate
        .config
        .as_ref()
        .expect("TLS config with client timeout");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(
        config.listeners[0].downstream_timeouts.client_timeout_ms,
        Some(30_000)
    );
}

#[test]
fn exact_http_tls_default_route_finalizes_with_an_unbounded_body_policy() {
    let certificate_path = fixture_path("tls-chain.pem");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display()
    );
    let lowered = import_fixture("strict-http-tls.cfg", source.as_bytes());

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let config = lowered
        .value()
        .config
        .as_ref()
        .expect("finalized strict HTTP TLS");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.listeners[0].protocol, Protocol::Http);
    assert!(config.listeners[0].tls_profile.is_some());
    assert_eq!(config.http_services[0].max_request_body_bytes, None);
    for path in ["/certificates/0", "/tls_profiles/0", "/listeners/0"] {
        assert_has_provenance(lowered.value(), path);
    }
}

#[test]
fn tls_sidecar_key_must_match_the_leaf_certificate() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempdir().expect("TLS identity directory");
    let certificate_path = directory.path().join("proxy.pem");
    let private_key_path = directory.path().join("proxy.pem.key");
    fs::copy(fixture_path("tls-chain.pem"), &certificate_path).expect("copy certificate chain");
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nginx/proxy-mismatched-key.pem"),
        &private_key_path,
    )
    .expect("copy mismatched key");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o600))
        .expect("secure mismatched key mode");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display()
    );

    let lowered = import_fixture("mismatched-tls.cfg", source.as_bytes());
    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.certificates.is_empty());
    assert!(diagnostic_contains(
        lowered.diagnostics(),
        "does not match the leaf certificate"
    ));
}

#[test]
fn repeated_tls_bundle_is_deduplicated_across_canonical_listeners() {
    let certificate_path = fixture_path("tls-chain.pem");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout client 30s\n  timeout server 30s\nfrontend first\n  bind 127.0.0.1:8443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nfrontend second\n  bind 127.0.0.1:9443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display(),
        certificate_path.display()
    );
    let resolved = analyze_fixture("reused-tls.cfg", source.as_bytes());
    let first = resolved.value().frontends[0].binds[0]
        .tls
        .as_ref()
        .expect("first TLS bind");
    let second = resolved.value().frontends[1].binds[0]
        .tls
        .as_ref()
        .expect("second TLS bind");
    assert_eq!(first.value, second.value);

    let lowered = import_fixture("reused-tls.cfg", source.as_bytes());
    let candidate = lowered.value();

    let config = candidate.config.as_ref().expect("reused TLS config");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.tls_profiles.len(), 2);
    assert_eq!(config.listeners.len(), 2);
    assert!(
        candidate
            .provenance
            .iter()
            .any(|provenance| provenance.path.starts_with("/certificates/"))
    );
}

#[test]
fn tls_bind_with_no_dns_identities_never_emits_a_listener_or_empty_certificate() {
    let certificate_path = fixture_path("tls-no-identities.pem");
    let source = format!(
        "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn http/1.1\n  maxconn 100\n",
        certificate_path.display()
    );
    let lowered = import_fixture("tls-empty-identities.cfg", source.as_bytes());

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.certificates.is_empty());
    assert!(lowered.value().draft.tls_profiles.is_empty());
    assert!(lowered.value().draft.listeners.is_empty());
    assert!(diagnostic_contains(
        lowered.diagnostics(),
        "no DNS subject alternative names"
    ));
}

#[test]
fn crt_list_and_multiple_crt_parameters_are_blocked_without_guessing() {
    let certificate_path = fixture_path("tls-chain.pem");
    let sources = [
        format!(
            "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt-list {}\n",
            certificate_path.display()
        ),
        format!(
            "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} crt {}\n",
            certificate_path.display(),
            certificate_path.display()
        ),
    ];

    for source in sources {
        let lowered = import_fixture("unsupported-certs.cfg", source.as_bytes());
        assert!(lowered.value().config.is_none());
        assert!(lowered.value().draft.certificates.is_empty());
        assert!(lowered.value().draft.listeners.is_empty());
        assert!(diagnostic_contains(
            lowered.diagnostics(),
            "certificate selection"
        ));
    }
}

#[test]
fn oversized_certificate_metadata_is_blocked_before_a_tls_listener_is_emitted() {
    let temp = tempdir().expect("temporary directory");
    let certificate_path = temp.path().join("oversized.pem");
    fs::write(&certificate_path, vec![b'x'; 1024 * 1024 + 1]).expect("oversized certificate");
    let source = format!(
        "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn http/1.1\n  maxconn 100\n",
        certificate_path.display()
    );
    let lowered = import_fixture("oversized-tls.cfg", source.as_bytes());

    assert!(lowered.value().draft.certificates.is_empty());
    assert!(lowered.value().draft.listeners.is_empty());
    assert!(diagnostic_contains(
        lowered.diagnostics(),
        "exceeds 1048576 bytes"
    ));
}

fn routing_fixture() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8080\n  maxconn 100\n  acl api_path path_beg /api\n  use_backend api if api_path\n  default_backend fallback\nbackend api\n  balance roundrobin\n  server api1 127.0.0.1:3001\nbackend fallback\n  balance roundrobin\n  server fallback1 127.0.0.1:3003\n"
        .into()
}

fn loaded_fixture(name: &str, contents: &[u8]) -> LoadedSource {
    let path = PathBuf::from(name);
    LoadedSource {
        root_ordinal: 0,
        file_ordinal: 0,
        source: SourceFile::from_path(SourceId::new(0), path.clone(), contents),
        path,
    }
}

fn analyze_fixture(
    name: &str,
    contents: &[u8],
) -> Report<oxiroute_import::haproxy::EffectiveConfiguration> {
    analyze_sources(&[loaded_fixture(name, contents)])
}

fn import_fixture(name: &str, contents: &[u8]) -> Report<CanonicalCandidate> {
    import_sources(&[loaded_fixture(name, contents)])
}

fn import_fixture_with_options(
    name: &str,
    contents: &[u8],
    options: &HaproxyImportOptions,
) -> Report<CanonicalCandidate> {
    oxiroute_import::haproxy::import_parsed_with_options(
        oxiroute_import::haproxy::parse_sources(&[loaded_fixture(name, contents)]),
        options,
    )
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/haproxy")
        .join(name)
        .canonicalize()
        .expect("canonical fixture path")
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

fn assert_blocker(diagnostics: &[Diagnostic], message: &str) {
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == E_SEMANTICS_NOT_REPRESENTABLE
                && diagnostic.severity() == Severity::Error
                && diagnostic.primary_span().is_some()
                && diagnostic.message().contains(message)
        }),
        "missing blocker containing {message:?}"
    );
}

fn assert_hostrouter_endpoints(pool: &oxiroute_config::UpstreamPool) {
    let endpoints = pool
        .servers
        .iter()
        .map(|server| match &server.endpoint {
            UpstreamEndpoint::Dns { host, port } => (host.as_str(), *port),
            endpoint => panic!("unexpected hostrouter endpoint: {endpoint:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        endpoints,
        [
            ("whitebeast.lan", 11434),
            ("whitebeast.lan", 11435),
            ("bhavapower.lan", 11434),
            ("bhavapower.lan", 11435),
            ("phoenix.lan", 11434),
            ("chicopc.lan", 11434),
            ("chicopc.lan", 11435),
            ("back1.lan", 11434),
            ("back1.lan", 11435),
            ("macmini.lan", 11434),
        ]
    );
}

fn assert_has_provenance(candidate: &CanonicalCandidate, path: &str) {
    assert!(
        candidate
            .provenance
            .iter()
            .any(|provenance| provenance.path == path),
        "missing provenance for {path}"
    );
}

fn assert_no_fallback_routes(candidate: &CanonicalCandidate) {
    assert!(candidate.draft.http_services.iter().all(|service| {
        service.routes.iter().all(|route| {
            matches!(&route.action, HttpRouteAction::Proxy { upstream_pool, .. }
                if matches!(upstream_pool.as_str(), "app_nodes" | "administration" | "phoenix_nodes"))
        })
    }));
}

fn diagnostic_contains(diagnostics: &[Diagnostic], message: &str) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message().contains(message))
}

fn code_count(diagnostics: &[Diagnostic], code: oxiroute_import::DiagnosticCode) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == code)
        .count()
}
