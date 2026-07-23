use std::{fmt::Write as _, path::PathBuf, time::Duration};

use oxiroute_import::haproxy::{
    AclCriterion, BalanceAlgorithm, BindAddress, BlockingReason, Configuration, DefaultsSelection,
    E_CONFLICTING_DIRECTIVE, E_DUPLICATE_IDENTITY, E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED,
    E_STATS_UNSUPPORTED, E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FORM, E_UNSUPPORTED_SECTION, EffectiveConfiguration, Externalization,
    LoadedSource, OptionState, ProxyMode, SemanticBlockerKind, ServerAddress, parse_sources,
    resolve_parsed,
};
use oxiroute_import::{DiagnosticStage, Report, Severity, SourceFile, SourceId};

#[test]
fn resolves_hostrouter_unix_frontend_and_dns_backend_without_lowering() {
    let contents = b"global
  log /dev/log local0
  chroot /var/lib/haproxy
  user haproxy
  group haproxy
  daemon
  maxconn 4096
  stats socket /run/haproxy/admin.sock
defaults shared
  log global
  mode http
  option httplog
  option forwardfor except 127.0.0.0/8
  option httpchk GET /healthz
  http-check expect status 200
  retries 3
  option redispatch
  timeout connect 5s
  timeout client 50s
  timeout server 50s
  timeout http-request 10s
  timeout http-keep-alive 10s
frontend hostrouter
  bind /run/haproxy/hostrouter.sock
  maxconn 2000
  acl host_app hdr(host) -i app.lan
  use_backend app_nodes if host_app
  default_backend app_nodes
backend app_nodes
  balance leastconn
  server app1 app1.lan:3000 check inter 2s rise 2 fall 3
  server app2 app2.lan:3000 check inter 2s rise 2 fall 3
";
    let (configuration, report) = resolve_bytes(contents);

    assert_eq!(report.value().global.maxconn.as_ref().unwrap().value, 4096);
    assert_eq!(report.value().frontends.len(), 1);
    assert_eq!(report.value().backends.len(), 1);
    assert_hostrouter_frontend(report.value());
    assert_hostrouter_backend(report.value());

    assert_eq!(code_count(&report, E_LOGGING_UNSUPPORTED), 3);
    assert_eq!(code_count(&report, E_PROCESS_OWNED), 4);
    assert_eq!(code_count(&report, E_STATS_UNSUPPORTED), 1);
    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
    assert_eq!(code_count(&report, E_UNCONSUMED_DIRECTIVE), 0);
}

fn assert_hostrouter_frontend(configuration: &EffectiveConfiguration) {
    let frontend = &configuration.frontends[0];
    assert_eq!(
        frontend.binds[0].value,
        BindAddress::Unix {
            path: b"/run/haproxy/hostrouter.sock".to_vec()
        }
    );
    assert_eq!(
        frontend.settings.mode.as_ref().unwrap().value,
        ProxyMode::Http
    );
    assert!(
        frontend
            .settings
            .mode
            .as_ref()
            .unwrap()
            .provenance
            .is_inherited()
    );
    assert_eq!(frontend.settings.maxconn.as_ref().unwrap().value, 2000);
    assert_eq!(frontend.acls[0].value.criterion, AclCriterion::HostExact);
    assert!(frontend.acls[0].value.case_insensitive);
    assert_eq!(frontend.use_backends.len(), 1);
    assert_eq!(frontend.use_backends[0].value.backend.name, b"app_nodes");
    assert_eq!(
        frontend
            .settings
            .default_backend
            .as_ref()
            .unwrap()
            .value
            .name,
        b"app_nodes"
    );
    assert!(
        frontend
            .settings
            .default_backend
            .as_ref()
            .unwrap()
            .provenance
            .is_reference()
    );
}

fn assert_hostrouter_backend(configuration: &EffectiveConfiguration) {
    let backend = &configuration.backends[0];
    assert_eq!(
        backend.settings.balance.as_ref().unwrap().value,
        BalanceAlgorithm::LeastConnections
    );
    assert_eq!(backend.settings.retries.as_ref().unwrap().value, 3);
    assert_eq!(
        backend.settings.timeouts.connect.as_ref().unwrap().value,
        Duration::from_secs(5)
    );
    assert_eq!(
        backend.settings.timeouts.server.as_ref().unwrap().value,
        Duration::from_secs(50)
    );
    assert_eq!(
        backend.settings.http_check_expect.as_ref().unwrap().value[0].start,
        200
    );
    assert!(matches!(
        backend.settings.redispatch.as_ref().unwrap().value,
        OptionState::Enabled(_)
    ));
    assert!(matches!(
        backend.settings.forward_for.as_ref().unwrap().value,
        OptionState::Enabled(ref forward_for)
            if forward_for.except.as_deref() == Some(b"127.0.0.0/8".as_slice())
    ));
    assert_eq!(backend.servers.len(), 2);
    assert_eq!(
        backend.servers[0].address.value,
        ServerAddress::Tcp {
            host: b"app1.lan".to_vec(),
            port: 3000
        }
    );
    assert_eq!(
        backend.servers[0].interval.as_ref().unwrap().value,
        Duration::from_secs(2)
    );
    assert_eq!(backend.servers[0].rise.as_ref().unwrap().value, 2);
    assert_eq!(backend.servers[0].fall.as_ref().unwrap().value, 3);
}

#[test]
fn resolves_dormant_phoenix_shape_with_eight_servers() {
    let mut contents = String::from(
        "defaults phoenix\n  mode http\n  option forwardfor\n  timeout connect 2s\n  timeout client 30s\n  timeout server 30s\nfrontend phoenix\n  bind /run/haproxy/phoenix.sock\n  acl phoenix_host hdr(host) -i phoenix.lan\n  use_backend phoenix_nodes if phoenix_host\nbackend phoenix_nodes\n  balance leastconn\n",
    );
    for ordinal in 1..=8 {
        writeln!(
            contents,
            "  server phoenix{ordinal} phoenix{ordinal}.lan:4000 check inter 3s rise 2 fall 3"
        )
        .unwrap();
    }
    let (_, report) = resolve_bytes(contents.as_bytes());

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().backends[0].servers.len(), 8);
    assert_eq!(
        report.value().frontends[0].use_backends[0]
            .value
            .backend
            .name,
        b"phoenix_nodes"
    );
    assert!(
        report.value().backends[0]
            .settings
            .forward_for
            .as_ref()
            .unwrap()
            .provenance
            .is_inherited()
    );
}

#[test]
fn resolves_listen_as_one_combined_frontend_and_backend_occurrence() {
    let contents = b"defaults tcp_defaults
  mode tcp
  timeout connect 2s
  timeout client 30s
  timeout server 30s
listen postgres
  bind :5432
  balance roundrobin
  maxconn 128
  server primary postgres.lan:5432 check inter 5s rise 2 fall 3
";
    let (_, report) = resolve_bytes(contents);

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().listens.len(), 1);
    let listen = &report.value().listens[0];
    assert_eq!(listen.settings.mode.as_ref().unwrap().value, ProxyMode::Tcp);
    assert_eq!(
        listen.settings.balance.as_ref().unwrap().value,
        BalanceAlgorithm::RoundRobin
    );
    assert_eq!(listen.settings.maxconn.as_ref().unwrap().value, 128);
    assert_eq!(
        listen.binds[0].value,
        BindAddress::Tcp {
            host: Vec::new(),
            port: 5432,
        }
    );
    assert_eq!(listen.servers.len(), 1);
}

#[test]
fn defaults_chains_preserve_source_order_and_exact_inheritance_steps() {
    let contents = b"defaults base
  mode tcp
  timeout connect 1s
defaults web from base
  mode http
  timeout server 2s
frontend implicit_web
  bind :80
defaults latest
  mode tcp
frontend implicit_latest
  bind :81
frontend explicit_web from web
  bind :82
";
    let (_, report) = resolve_bytes(contents);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        report
            .value()
            .defaults
            .iter()
            .map(|defaults| defaults.section.name.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [b"base".as_slice(), b"web".as_slice(), b"latest".as_slice()]
    );
    let web = &report.value().defaults[1];
    let inherited_connect = web.settings.timeouts.connect.as_ref().unwrap();
    assert_eq!(inherited_connect.provenance.inheritance.len(), 1);
    assert_eq!(
        inherited_connect.provenance.inheritance[0].selection,
        DefaultsSelection::Explicit
    );

    let implicit_web = &report.value().frontends[0];
    assert_eq!(
        implicit_web.defaults.as_ref().unwrap().selection,
        DefaultsSelection::ImplicitLatest
    );
    assert_eq!(
        implicit_web.settings.mode.as_ref().unwrap().value,
        ProxyMode::Http
    );
    assert_eq!(
        implicit_web
            .settings
            .timeouts
            .connect
            .as_ref()
            .unwrap()
            .provenance
            .inheritance
            .len(),
        2
    );

    let implicit_latest = &report.value().frontends[1];
    assert_eq!(
        implicit_latest.settings.mode.as_ref().unwrap().value,
        ProxyMode::Tcp
    );
    assert!(implicit_latest.settings.timeouts.connect.is_none());

    let explicit_web = &report.value().frontends[2];
    assert_eq!(
        explicit_web.defaults.as_ref().unwrap().selection,
        DefaultsSelection::Explicit
    );
    assert_eq!(
        explicit_web.settings.mode.as_ref().unwrap().value,
        ProxyMode::Http
    );
    assert_eq!(
        explicit_web
            .settings
            .timeouts
            .connect
            .as_ref()
            .unwrap()
            .provenance
            .inheritance
            .len(),
        2
    );
}

#[test]
fn parsed_source_vector_order_controls_latest_defaults_across_files() {
    let parsed = parse_sources(&[
        loaded(7, "first.cfg", b"defaults first\n  mode tcp\n"),
        loaded(2, "second.cfg", b"defaults second\n  mode http\n"),
        loaded(
            9,
            "third.cfg",
            b"frontend public\n  bind /run/haproxy/public.sock\n",
        ),
    ]);
    let report = resolve_parsed(parsed);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        report.value().frontends[0]
            .defaults
            .as_ref()
            .unwrap()
            .section
            .source,
        SourceId::new(2)
    );
    assert_eq!(
        report.value().frontends[0]
            .settings
            .mode
            .as_ref()
            .unwrap()
            .value,
        ProxyMode::Http
    );
}

#[test]
fn explicit_defaults_reference_requires_one_unique_target() {
    let contents = b"defaults shared
  mode http
defaults shared
  mode tcp
frontend ambiguous from shared
  bind :80
frontend missing from absent
  bind :81
";
    let (_, report) = resolve_bytes(contents);

    assert_eq!(code_count(&report, E_DUPLICATE_IDENTITY), 1);
    assert_eq!(code_count(&report, E_UNRESOLVED_REFERENCE), 2);
    assert!(
        report
            .value()
            .frontends
            .iter()
            .all(|frontend| frontend.defaults.is_none() && frontend.settings.mode.is_none())
    );
}

#[test]
fn duplicate_identities_are_blocking_and_never_overwrite_occurrences() {
    let contents = b"defaults base
  mode http
frontend public
  bind :80
frontend public
  bind :81
backend app
  server node node1.lan:3000
  server node node2.lan:3000
backend app
  server other node3.lan:3000
";
    let (configuration, report) = resolve_bytes(contents);

    assert_eq!(code_count(&report, E_DUPLICATE_IDENTITY), 3);
    assert_eq!(report.value().frontends.len(), 2);
    assert_eq!(report.value().backends.len(), 2);
    assert_eq!(report.value().backends[0].servers.len(), 2);
    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
    assert!(report.value().ledger.iter().any(|decision| {
        decision.outcome
            == oxiroute_import::haproxy::DecisionOutcome::Blocked(BlockingReason::DuplicateIdentity)
    }));
}

#[test]
fn use_backend_rules_retain_first_match_source_order() {
    let contents = b"defaults web
  mode http
frontend public
  bind :80
  acl host_first hdr(host) -i first.lan
  acl host_second hdr(host) -i second.lan
  use_backend first_pool if host_first
  use_backend second_pool if host_second
  default_backend fallback
backend second_pool
  server second second.lan:3000
backend fallback
  server fallback fallback.lan:3000
backend first_pool
  server first first.lan:3000
";
    let (_, report) = resolve_bytes(contents);

    assert!(report.diagnostics().is_empty());
    let frontend = &report.value().frontends[0];
    assert_eq!(
        frontend
            .use_backends
            .iter()
            .map(|rule| rule.value.backend.name.as_slice())
            .collect::<Vec<_>>(),
        [b"first_pool".as_slice(), b"second_pool".as_slice()]
    );
    assert_eq!(
        frontend
            .use_backends
            .iter()
            .map(|rule| rule.value.condition.name.as_slice())
            .collect::<Vec<_>>(),
        [b"host_first".as_slice(), b"host_second".as_slice()]
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
    assert!(reasons.contains(&BlockingReason::Logging));
    assert!(externalized);
    assert!(reasons.contains(&BlockingReason::Statistics));
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
  retry-on conn-failure
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
    assert!(defaults.settings.semantic_blockers.iter().any(|blocker| {
        blocker.value.kind == SemanticBlockerKind::Logging && blocker.value.keyword == b"log-format"
    }));
    assert!(defaults.settings.semantic_blockers.iter().any(|blocker| {
        blocker.value.kind == SemanticBlockerKind::ProxyDefault
            && blocker.value.keyword == b"default-server"
    }));
    assert!(defaults.settings.semantic_blockers.iter().any(|blocker| {
        blocker.value.kind == SemanticBlockerKind::Retry && blocker.value.keyword == b"retry-on"
    }));
    assert!(defaults.settings.semantic_blockers.iter().any(|blocker| {
        blocker.value.kind == SemanticBlockerKind::Timeout && blocker.value.arguments[0] == b"queue"
    }));
    let server = &report.value().backends[0].servers[0];
    assert_eq!(
        server
            .unsupported_options
            .iter()
            .map(|option| option.value.name.as_slice())
            .collect::<Vec<_>>(),
        [
            b"weight".as_slice(),
            b"backup".as_slice(),
            b"maxconn".as_slice(),
            b"ssl".as_slice(),
            b"verify".as_slice(),
            b"sni".as_slice(),
            b"check-port".as_slice(),
            b"check-send-proxy".as_slice(),
        ]
    );
}

#[test]
fn conflicting_scalar_and_option_directives_are_blocking() {
    let contents = b"defaults shared
  mode http
  mode tcp
  timeout connect 1s
  timeout connect 2s
  option redispatch
  no option redispatch
frontend public
  bind 127.0.0.1:8080
";
    let (_, report) = resolve_bytes(contents);

    assert_eq!(code_count(&report, E_CONFLICTING_DIRECTIVE), 3);
    assert_eq!(
        report.value().defaults[0].settings.semantic_blockers.len(),
        3
    );
    assert!(
        report
            .value()
            .ledger
            .iter()
            .filter(|decision| {
                decision.outcome
                    == oxiroute_import::haproxy::DecisionOutcome::Blocked(
                        BlockingReason::ConflictingDirective,
                    )
            })
            .count()
            >= 3
    );
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
    assert_eq!(server.unsupported_options.len(), 2);
    assert_eq!(
        report.value().ledger.entries.last().unwrap().outcome,
        oxiroute_import::haproxy::DecisionOutcome::Blocked(BlockingReason::ConflictingDirective)
    );
}

#[test]
fn unsupported_modes_are_inherited_as_blockers_instead_of_falling_back_to_http() {
    let contents = b"defaults shared
  mode health
frontend public
  bind 127.0.0.1:8080
";
    let (_, report) = resolve_bytes(contents);
    let mode = report.value().frontends[0]
        .settings
        .mode
        .as_ref()
        .expect("retained unsupported mode");

    assert_eq!(mode.value, ProxyMode::Unsupported(b"health".to_vec()));
    assert!(mode.provenance.is_inherited());
    assert!(
        report.value().frontends[0]
            .settings
            .semantic_blockers
            .iter()
            .any(|blocker| blocker.value.kind == SemanticBlockerKind::Mode)
    );
}

fn resolve_bytes(contents: &[u8]) -> (Configuration, Report<EffectiveConfiguration>) {
    let parsed = parse_sources(&[loaded(0, "haproxy.cfg", contents)]);
    let configuration = parsed.value().clone();
    (configuration, resolve_parsed(parsed))
}

fn loaded(source_id: u32, name: &str, contents: &[u8]) -> LoadedSource {
    let path = PathBuf::from(name);
    LoadedSource {
        root_ordinal: 0,
        file_ordinal: usize::try_from(source_id).unwrap(),
        source: SourceFile::from_path(SourceId::new(source_id), path.clone(), contents),
        path,
    }
}

fn occurrence_count(configuration: &Configuration) -> usize {
    configuration
        .files
        .iter()
        .map(|file| {
            file.document.preamble.len()
                + file
                    .document
                    .sections
                    .iter()
                    .map(|section| 1 + section.directives.len())
                    .sum::<usize>()
        })
        .sum()
}

fn code_count(
    report: &Report<EffectiveConfiguration>,
    code: oxiroute_import::DiagnosticCode,
) -> usize {
    report
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code() == code)
        .count()
}
