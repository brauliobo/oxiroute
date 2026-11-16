use std::time::Duration;

use oxiroute_import::{
    DiagnosticStage, Severity,
    haproxy::{
        BlockingReason, E_PROCESS_OWNED, E_STATS_UNSUPPORTED, E_UNCONSUMED_DIRECTIVE,
        E_UNKNOWN_DIRECTIVE, E_UNRESOLVED_REFERENCE, E_UNSUPPORTED_FORM, E_UNSUPPORTED_SECTION,
        Externalization, SemanticBlockerKind, StatsAdminPolicy,
    },
};

use super::common::{code_count, occurrence_count, resolve_bytes};

#[test]
fn dedicated_stats_page_directives_are_typed_and_consumed_exactly() {
    let contents = b"frontend stats
  mode http
  bind *:8404
  stats uri /stats
  stats refresh 1500ms
  stats admin if LOCALHOST
  stats admin if LOCALHOST
";
    let (configuration, report) = resolve_bytes(contents);

    assert!(report.diagnostics().is_empty());
    let frontend = &report.value().frontends[0];
    assert!(frontend.stats.enable.as_ref().unwrap().value);
    assert_eq!(frontend.stats.uri_prefix.as_ref().unwrap().value, b"/stats");
    assert_eq!(
        frontend.stats.refresh.as_ref().unwrap().value,
        Duration::from_millis(1_500)
    );
    assert_eq!(
        frontend.stats.admin.as_ref().unwrap().value,
        StatsAdminPolicy::Localhost
    );
    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
    assert!(report.value().ledger.iter().all(|decision| {
        matches!(
            decision.outcome,
            oxiroute_import::haproxy::DecisionOutcome::Consumed(_)
        )
    }));
    assert!(report.value().activation_requirements.is_empty());
}

#[test]
fn unsupported_stats_forms_remain_externalized_and_block_page_activation() {
    let contents = b"frontend stats
  mode http
  bind *:8404
  stats enable
  stats uri /stats
  stats refresh 10s
  stats admin if TRUE
";
    let (_, report) = resolve_bytes(contents);
    let section = report.value().frontends[0].section.id;

    assert_eq!(code_count(&report, E_STATS_UNSUPPORTED), 1);
    assert_eq!(report.value().activation_requirements.len(), 1);
    assert!(
        report
            .value()
            .blocked_stats_page_sections
            .contains(&section)
    );
}

#[test]
fn every_unknown_or_unsupported_occurrence_gets_a_terminal_blocking_decision() {
    let contents = b".if defined(ENABLED)
global
  log /dev/log local0
  user haproxy
  stats socket /run/haproxy/admin.sock
  mystery value
defaults base
  mode http extra
  option magical
frontend public
  bind \"${ADDRESS-:80}\"
  default_backend missing
  .endif
cache objects
  total-max-size 32
";
    let (configuration, report) = resolve_bytes(contents);
    let reasons = report
        .value()
        .ledger
        .iter()
        .filter_map(|decision| match decision.outcome {
            oxiroute_import::haproxy::DecisionOutcome::Blocked(reason) => Some(reason),
            _ => None,
        })
        .collect::<Vec<_>>();
    let externalized = report.value().ledger.iter().any(|decision| {
        decision.outcome
            == oxiroute_import::haproxy::DecisionOutcome::Externalized(
                Externalization::ProcessOwned,
            )
    });

    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
    assert_eq!(code_count(&report, E_UNCONSUMED_DIRECTIVE), 0);
    assert_eq!(code_count(&report, E_UNKNOWN_DIRECTIVE), 1);
    assert_eq!(code_count(&report, E_UNRESOLVED_REFERENCE), 1);
    assert_eq!(code_count(&report, E_UNSUPPORTED_FORM), 2);
    assert_eq!(code_count(&report, E_UNSUPPORTED_SECTION), 1);
    assert!(reasons.contains(&BlockingReason::ConditionalPreprocessing));
    assert!(reasons.contains(&BlockingReason::EnvironmentPreprocessing));
    assert!(!reasons.contains(&BlockingReason::Logging));
    assert!(externalized);
    assert!(!reasons.contains(&BlockingReason::Statistics));
    assert_eq!(report.value().activation_requirements.len(), 1);
    assert!(report.value().deployment_requirements.len() >= 2);
    assert!(reasons.contains(&BlockingReason::UnknownDirective));
    assert!(reasons.contains(&BlockingReason::UnresolvedReference));
    assert!(reasons.contains(&BlockingReason::UnsupportedForm));
    assert!(reasons.contains(&BlockingReason::UnsupportedSection));
    assert!(report.diagnostics().iter().all(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Resolve && diagnostic.primary_span().is_some()
    }));
    assert!(report.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_PROCESS_OWNED && diagnostic.severity() == Severity::Warning
    }));
}

#[test]
fn semantically_relevant_unsupported_settings_are_retained_as_blockers() {
    let contents = b"global
  ssl-default-bind-options ssl-min-ver TLSv1.3
defaults shared
  mode http
  log-format custom
  default-server weight 20
  default-server ssl
  retry-on 0rtt-rejected
  timeout queue 5s
frontend public
  bind 127.0.0.1:8080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000 weight 50 backup maxconn 10 ssl verify required sni req.hdr(host) check-port 3001 check-send-proxy
";
    let (_, report) = resolve_bytes(contents);

    assert!(report.has_errors());
    assert_eq!(report.value().global.semantic_blockers.len(), 1);
    assert_eq!(
        report.value().global.semantic_blockers[0].value.kind,
        SemanticBlockerKind::GlobalSecurity
    );
    let defaults = &report.value().defaults[0];
    assert!(
        report
            .value()
            .deployment_requirements
            .iter()
            .any(|requirement| requirement.directive == "log-format")
    );
    assert!(defaults.settings.semantic_blockers.iter().any(|blocker| {
        blocker.value.kind == SemanticBlockerKind::ProxyDefault
            && blocker.value.keyword == b"default-server"
    }));
    assert!(defaults.settings.semantic_blockers.iter().any(|blocker| {
        blocker.value.kind == SemanticBlockerKind::Retry && blocker.value.keyword == b"retry-on"
    }));
    assert_eq!(
        defaults
            .settings
            .timeouts
            .queue
            .as_ref()
            .expect("queue timeout")
            .value
            .as_secs(),
        5
    );
    let server = &report.value().backends[0].servers[0];
    assert_eq!(server.max_connections.as_ref().unwrap().value, 10);
    assert_eq!(
        report.value().defaults[0]
            .server_defaults
            .as_ref()
            .expect("default-server weight")
            .weight
            .as_ref()
            .expect("effective default-server weight")
            .value,
        20
    );
    assert_eq!(server.weight.as_ref().expect("server weight").value, 50);
    assert_eq!(
        server
            .unsupported_options
            .iter()
            .map(|option| option.value.name.as_slice())
            .collect::<Vec<_>>(),
        [
            b"backup".as_slice(),
            b"ssl".as_slice(),
            b"verify".as_slice(),
            b"sni".as_slice(),
            b"check-port".as_slice(),
            b"check-send-proxy".as_slice(),
        ]
    );
}
