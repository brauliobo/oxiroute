use std::net::SocketAddr;

use oxiroute_config::{
    HttpHostSelector, HttpRouteAction, ListenerBind, Protocol, UpstreamAlgorithm, UpstreamEndpoint,
};
use oxiroute_import::{
    E_SEMANTICS_NOT_REPRESENTABLE, Severity,
    haproxy::{E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED},
};

use super::common::{
    MINIMAL, assert_blocker, assert_has_provenance, code_count, diagnostic_contains,
    import_fixture, import_synthetic, validated_config,
};

#[test]
fn unix_gateway_finalizes_with_exact_compatibility_policy() {
    let report = import_synthetic("unix-gateway.cfg", false);
    let candidate = report.value();
    let config = validated_config(candidate);

    assert!(!report.has_errors(), "{:#?}", report.diagnostics());
    assert_eq!(
        code_count(report.diagnostics(), E_SEMANTICS_NOT_REPRESENTABLE),
        0
    );
    assert_eq!(code_count(report.diagnostics(), E_LOGGING_UNSUPPORTED), 2);
    assert_eq!(code_count(report.diagnostics(), E_PROCESS_OWNED), 2);
    assert!(
        report
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.severity() == Severity::Warning)
    );
    assert!(!diagnostic_contains(
        report.diagnostics(),
        "Unix bind sockets"
    ));
    assert!(candidate.activation_requirements.is_empty());
    assert!(candidate.operational_overlays.is_empty());
    assert_eq!(candidate.summary().max_connections, Some(4096));
    assert_has_provenance(candidate, "/max_connections");
    assert_stats_page(config);

    assert_eq!(config.listeners.len(), 1);
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/var/run/synthetic-gateway.sock".into(),
            mode: Some(0o777),
        }
    );
    let timeouts = &config.listeners[0].downstream_timeouts;
    assert_eq!(timeouts.client_timeout_ms, Some(600_000));
    assert_eq!(timeouts.request_timeout_ms, Some(600_000));
    assert_eq!(timeouts.keepalive_timeout_ms, Some(60_000));

    let service = &config.http_services[0];
    assert_eq!(service.routes.len(), 2);
    assert!(matches!(
        service.routes[0].host,
        Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority { ref value })
            if value == "api.corp-a.test"
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
    assert_eq!(pool.queue_timeout_ms, None);
    let health = pool.health_check.as_ref().expect("HTTP health check");
    assert_eq!(health.interval_ms, 10_000);
    assert_eq!(health.timeout_ms, 10_000);
    assert_eq!(health.healthy_threshold, 2);
    assert_eq!(health.unhealthy_threshold, 3);
    assert_eq!(
        dns_endpoints(pool),
        [
            ("api01.example.net", 8080),
            ("api02.example.net", 8080),
            ("api03.example.net", 8081)
        ]
    );
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
            oxiroute_config::HttpRetryTrigger::ConnectTimeout
        ]
    );
}

fn assert_stats_page(config: &oxiroute_config::ConfigDraft) {
    let stats = config.stats.as_ref().expect("HAProxy stats page");
    assert_eq!(stats.pages.len(), 1);
    assert_eq!(stats.pages[0].bind, "0.0.0.0:8404".parse().unwrap());
    assert_eq!(stats.pages[0].uri_prefix, "/stats");
    assert_eq!(stats.pages[0].refresh_ms, 10_000);
    assert_eq!(
        stats.pages[0].admin,
        oxiroute_config::StatsPageAdminPolicy::Localhost
    );
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
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Unix mode config");

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
fn minimal_static_tcp_fixture_finalizes_and_validates() {
    let lowered = import_fixture("minimal-representable.cfg", MINIMAL);

    assert!(lowered.diagnostics().is_empty());
    let candidate = lowered.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized config");
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
            address: "127.0.0.1:15432".parse::<SocketAddr>().unwrap(),
            ipv6_only: false
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
    let independently_validated = config.clone().validate().expect("canonical validation");
    assert_eq!(independently_validated.as_draft(), config);
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
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
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
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
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

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(lowered.value().summary().listeners, 0);
    assert_eq!(lowered.value().summary().l4_services, 0);
    assert_eq!(lowered.value().summary().upstream_pools, 1);
    assert_blocker(lowered.diagnostics(), "timeout connect must be explicit");
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
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized listen");

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

    assert!(
        candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some()
    );
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

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(lowered.value().summary().listeners, 0);
    assert_eq!(lowered.value().summary().http_services, 0);
    assert_blocker(lowered.diagnostics(), "unsupported HAProxy mode");
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

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(lowered.value().summary().listeners, 0);
    assert_eq!(lowered.value().summary().l4_services, 0);
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

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(lowered.value().summary().listeners, 0);
    assert_eq!(lowered.value().summary().l4_services, 0);
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
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
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
    assert!(
        aggregate
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(aggregate.value().summary().listeners, 0);
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
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("global admission limit");
    assert!(!config.listeners.is_empty(), "{global_fallback:#?}");
    assert_eq!(config.max_connections, Some(500));
    assert_eq!(config.listeners[0].max_connections, Some(100));
    assert_has_provenance(global_fallback.value(), "/max_connections");

    let bind_cap = import_fixture("bind-admission.cfg", bind_cap);
    let config = bind_cap
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("exact bind admission");
    assert_eq!(config.listeners[0].max_connections, Some(75));
    assert_has_provenance(bind_cap.value(), "/listeners/0/max_connections");
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
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
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
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("per-socket admission");
    assert_eq!(per_socket_config.listeners.len(), 2);
    assert!(
        per_socket_config
            .listeners
            .iter()
            .all(|listener| listener.max_connections == Some(40))
    );
}

fn dns_endpoints(pool: &oxiroute_config::UpstreamPool) -> Vec<(&str, u16)> {
    pool.servers
        .iter()
        .map(|server| match &server.endpoint {
            UpstreamEndpoint::Dns { host, port } => (host.as_str(), *port),
            endpoint => panic!("unexpected non-DNS endpoint: {endpoint:?}"),
        })
        .collect()
}
