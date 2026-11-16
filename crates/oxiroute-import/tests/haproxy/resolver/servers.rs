use oxiroute_import::haproxy::{
    BlockingReason, E_CONFLICTING_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNSUPPORTED_FORM,
};

use super::common::{code_count, occurrence_count, resolve_bytes};

#[test]
fn resolves_http_check_send_as_a_typed_canonical_request() {
    let contents = b"defaults web
  mode http
  option httpchk
  timeout connect 5s
  timeout server 30s
  default-server check inter 2s rise 2 fall 3
backend app
  balance roundrobin
  http-check send meth GET uri /healthz ver HTTP/1.1 hdr Host app.internal
  http-check expect status 204
   server app1 127.0.0.1:3000
";
    let (configuration, report) = resolve_bytes(contents);

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let backend = &report.value().backends[0];
    let send = &backend
        .settings
        .http_check_send
        .as_ref()
        .expect("typed send request")
        .value;
    assert_eq!(send.method, b"GET");
    assert_eq!(send.uri, b"/healthz");
    assert_eq!(send.version, b"HTTP/1.1");
    assert_eq!(send.host.as_deref(), Some(b"app.internal".as_slice()));
    assert_eq!(
        backend
            .settings
            .http_check_expect
            .as_ref()
            .expect("exact status")
            .value[0]
            .start,
        204
    );
    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
    assert!(report.value().ledger.iter().all(|decision| {
        matches!(
            decision.outcome,
            oxiroute_import::haproxy::DecisionOutcome::Consumed(_)
                | oxiroute_import::haproxy::DecisionOutcome::Superseded { .. }
        )
    }));
}

#[test]
fn unsupported_http_check_send_forms_remain_blocking() {
    for directive in [
        "http-check send meth GET uri /healthz ver HTTP/1.1 hdr X-Token secret",
        "http-check send meth GET uri /healthz ver HTTP/1.1 hdr Host %[src]",
        "http-check connect port 8443",
    ] {
        let contents = format!(
            "backend app\n  balance roundrobin\n  {directive}\n  server app1 127.0.0.1:3000 check\n"
        );
        let (_, report) = resolve_bytes(contents.as_bytes());

        assert_eq!(code_count(&report, E_UNSUPPORTED_FORM), 1, "{directive}");
        assert_eq!(code_count(&report, E_UNKNOWN_DIRECTIVE), 0, "{directive}");
        assert!(report.value().ledger.iter().any(|decision| {
            decision.keyword == b"http-check"
                && decision.outcome
                    == oxiroute_import::haproxy::DecisionOutcome::Blocked(
                        BlockingReason::UnsupportedForm,
                    )
        }));
    }
}

#[test]
fn bounded_server_weights_resolve_as_effective_values() {
    let contents = b"defaults shared
  mode tcp
  default-server weight 20
backend app
  balance roundrobin
  server app1 127.0.0.1:3000 weight 50
  server app2 127.0.0.1:3001
";
    let (_, report) = resolve_bytes(contents);
    let servers = &report.value().backends[0].servers;

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert_eq!(servers[0].weight.as_ref().expect("direct weight").value, 50);
    assert_eq!(
        servers[1].weight.as_ref().expect("inherited weight").value,
        20
    );
    assert!(
        servers
            .iter()
            .all(|server| server.unsupported_options.is_empty())
    );
}

#[test]
fn invalid_server_weights_remain_blocking() {
    for value in ["0", "101", "+5", "50%", "%[var(txn.weight)]"] {
        let contents = format!(
            "backend app\n  balance roundrobin\n  server app1 127.0.0.1:3000 weight {value}\n"
        );
        let (_, report) = resolve_bytes(contents.as_bytes());

        assert!(report.has_errors(), "invalid weight resolved: {value}");
        assert_eq!(code_count(&report, E_UNSUPPORTED_FORM), 1, "{value}");
    }
}

#[test]
fn conflicting_server_options_are_reported_without_discarding_the_server() {
    let contents = b"backend app
  server app1 127.0.0.1:3000 weight 10 weight 20 verify none verify required
";
    let (_, report) = resolve_bytes(contents);
    let server = &report.value().backends[0].servers[0];

    assert_eq!(code_count(&report, E_CONFLICTING_DIRECTIVE), 2);
    assert_eq!(server.name.value, b"app1");
    assert_eq!(server.unsupported_options.len(), 1);
    assert_eq!(server.unsupported_options[0].value.name, b"verify");
    assert_eq!(
        report.value().ledger.entries.last().unwrap().outcome,
        oxiroute_import::haproxy::DecisionOutcome::Blocked(BlockingReason::ConflictingDirective)
    );
}
