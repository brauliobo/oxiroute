use std::{fs, net::SocketAddr, path::PathBuf};

use oxiroute_config::{
    HttpHostSelector, HttpPathSelector, HttpRouteAction, ListenerBind, Protocol, UpstreamAlgorithm,
    UpstreamEndpoint, validate_config,
};
use oxiroute_import::{
    Diagnostic, DiagnosticStage, E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, Report, Severity,
    SourceFile, SourceId,
    haproxy::{
        CanonicalCandidate, E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION,
        E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED, E_STATS_UNSUPPORTED, E_UNKNOWN_DIRECTIVE,
        E_UNSUPPORTED_FORM, LoadedSource, analyze_sources, import_sources,
    },
};
use tempfile::tempdir;

const HOSTROUTER: &[u8] = include_bytes!("fixtures/haproxy/hostrouter-active.cfg");
const SYNTHETIC_UNIX_DNS_LEASTCONN: &[u8] =
    include_bytes!("fixtures/haproxy/synthetic-unix-dns-leastconn.cfg");
const PHOENIX: &[u8] = include_bytes!("fixtures/haproxy/phoenix-dormant.cfg");
const MINIMAL: &[u8] = include_bytes!("fixtures/haproxy/minimal-representable.cfg");

#[test]
fn hostrouter_active_report_retains_every_audited_activation_blocker() {
    let lowered = import_fixture("hostrouter-active.cfg", HOSTROUTER);
    let candidate = lowered.value();

    assert!(candidate.config.is_none());
    assert!(candidate.draft.upstream_pools.is_empty());
    assert!(
        candidate
            .draft
            .listeners
            .iter()
            .all(|listener| listener.name != "hostrouter")
    );
    assert_eq!(code_count(lowered.diagnostics(), E_LOGGING_UNSUPPORTED), 3);
    assert_eq!(code_count(lowered.diagnostics(), E_STATS_UNSUPPORTED), 6);
    assert_eq!(code_count(lowered.diagnostics(), E_PROCESS_OWNED), 4);
    assert_process_settings_are_external_warnings(lowered.diagnostics());
    assert_blocker(lowered.diagnostics(), "aggregate process limit");
    assert_blocker(lowered.diagnostics(), "leastconn");
    assert_blocker(lowered.diagnostics(), "initially eligible");
    assert_blocker(lowered.diagnostics(), "HAProxy retries");
    assert_blocker(lowered.diagnostics(), "redispatch persistence");
    assert_blocker(lowered.diagnostics(), "timeout scope");
    assert_blocker(lowered.diagnostics(), "forwardfor header insertion");
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "Unix bind sockets"
    ));
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "DNS-named servers"
    ));
    assert!(candidate.draft.http_services.is_empty());
    assert_no_fallback_routes(candidate);
}

#[test]
fn phoenix_dormant_report_cannot_activate_or_substitute_its_dns_pool() {
    let lowered = import_fixture("phoenix-dormant.cfg", PHOENIX);

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.upstream_pools.is_empty());
    assert!(lowered.value().draft.listeners.is_empty());
    assert_eq!(code_count(lowered.diagnostics(), E_PROCESS_OWNED), 4);
    assert_blocker(lowered.diagnostics(), "leastconn");
    assert_blocker(lowered.diagnostics(), "initially eligible");
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
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::RoundRobin
    );
    assert_eq!(
        config.upstream_pools[0].endpoints,
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
        "/upstream_pools/0/endpoints/0/type",
        "/upstream_pools/0/endpoints/0/address",
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
            path: "/run/haproxy/hostrouter.sock".into()
        }
    );
    assert_eq!(config.listeners[0].max_connections, Some(1500));
    assert_eq!(
        config.upstream_pools[0].endpoints,
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
        "/upstream_pools/0/endpoints/0/type",
        "/upstream_pools/0/endpoints/0/host",
        "/upstream_pools/0/endpoints/0/port",
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
        config.upstream_pools[0].endpoints,
        [UpstreamEndpoint::Unix {
            path: "/run/postgresql/.s.PGSQL.5432".into()
        }]
    );
    assert_has_provenance(lowered.value(), "/upstream_pools/0/endpoints/0/path");
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
fn case_insensitive_raw_acl_remains_blocking() {
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
  default_backend fallback
backend app
  balance roundrobin
  server app1 127.0.0.1:3001
backend fallback
  balance roundrobin
  server fallback1 127.0.0.1:3002
";
    let lowered = import_fixture("case-insensitive-host.cfg", source);

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.http_services.is_empty());
    assert!(lowered.value().draft.listeners.is_empty());
    assert_blocker(lowered.diagnostics(), "case-insensitive HAProxy ACL");
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
fn global_fallback_blocks_while_bind_maxconn_lowers_to_an_exact_optional_cap() {
    let global_fallback = b"global
  maxconn 500
defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend database
  bind 127.0.0.1:15432
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
    assert!(global_fallback.value().config.is_none());
    assert!(global_fallback.value().draft.listeners.is_empty());
    assert_blocker(global_fallback.diagnostics(), "aggregate process limit");

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
fn leastconn_remains_blocking_for_http_request_and_connection_accounting() {
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

    assert!(lowered.value().config.is_none());
    assert!(lowered.value().draft.upstream_pools.is_empty());
    assert!(lowered.value().draft.http_services.is_empty());
    assert!(lowered.value().draft.listeners.is_empty());
    assert_blocker(lowered.diagnostics(), "complete TCP endpoint set");
    assert!(!diagnostic_contains(
        lowered.diagnostics(),
        "request-body limit"
    ));
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
fn tls_bind_retains_pem_san_identities_and_sidecar_key_without_emitting_http_policy() {
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

    assert!(candidate.draft.certificates.is_empty());
    assert!(candidate.draft.tls_profiles.is_empty());
    assert!(candidate.draft.listeners.is_empty());
    assert!(candidate.draft.http_services.is_empty());
    assert_blocker(lowered.diagnostics(), "downstream/request timeout scope");
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
fn repeated_tls_bundle_is_retained_deterministically_without_canonical_claims() {
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

    assert!(candidate.draft.certificates.is_empty());
    assert!(candidate.draft.tls_profiles.is_empty());
    assert!(candidate.draft.listeners.is_empty());
    assert!(
        candidate
            .provenance
            .iter()
            .all(|provenance| !provenance.path.starts_with("/certificates/"))
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
