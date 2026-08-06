#![cfg(unix)]

use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use oxiroute_import::{
    DiagnosticStage, E_INCLUDE_CYCLE, E_UNSUPPORTED_FEATURE, SourceFile, SourceId,
    squid::{
        AccessAction, AccessEvaluation, AclReferenceResolution, AclType, Activation,
        AuthenticationValue, BuiltinAcl, DecisionOutcome, DirectiveFamily, DirectiveResolution,
        DirectiveSemantics, E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNSUPPORTED_FORM,
        ForwardedForMode, LogDestination, PeerOption, PortEndpoint, PrivacyDirective,
        RootSelectionSource, SecretKind, SemanticBlockerKind, SquidLoadLimits,
        SquidLoweringAdapter, discover_root, import, import_selected, lex, load, load_with_limits,
        parse,
    },
};
use tempfile::tempdir;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/squid")
        .join(name)
}

#[test]
fn canonical_lowering_fails_closed_for_unrepresented_defaults_and_connect_ranges() {
    let directory = tempdir().expect("fixture directory");
    for (name, source, expected) in [
        (
            "implicit-defaults.conf",
            "http_port 3128\nacl ssl_ports port 443\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\n",
            SemanticBlockerKind::ForwardedForPolicy,
        ),
        (
            "connect-range.conf",
            "http_port 3128\naccess_log none\nforwarded_for delete\nvia off\nacl ssl_ports port 443-444\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\n",
            SemanticBlockerKind::DestinationPortAcl,
        ),
        (
            "connect-allow-before-guard.conf",
            "http_port 3128\naccess_log none\nforwarded_for delete\nvia off\nacl ssl_ports port 443\nhttp_access allow localhost\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\n",
            SemanticBlockerKind::DestinationPortAcl,
        ),
        (
            "active-log.conf",
            "http_port 3128\naccess_log stdio:/tmp/access.log\nforwarded_for delete\nvia off\nacl ssl_ports port 443\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\n",
            SemanticBlockerKind::AccessLoggingPolicy,
        ),
        (
            "dns-control.conf",
            "http_port 3128\naccess_log none\nforwarded_for delete\nvia off\ndns_timeout 30 seconds\nacl ssl_ports port 443\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\n",
            SemanticBlockerKind::ResolverPolicy,
        ),
        (
            "https-port.conf",
            "http_port 3128\nhttps_port 3129\naccess_log none\nforwarded_for delete\nvia off\nacl ssl_ports port 443\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\n",
            SemanticBlockerKind::ForwardProxyListener,
        ),
        (
            "unsupported-auth-setting.conf",
            "http_port 3128\naccess_log none\nforwarded_for delete\nvia off\nacl ssl_ports port 443\nhttp_access deny CONNECT !ssl_ports\nauth_param basic program /usr/lib/squid/basic_ncsa_auth /tmp/users\nauth_param basic realm Private proxy\nauth_param basic credentialsttl 2 hours\nauth_param basic utf8 on\nacl authenticated proxy_auth REQUIRED\nhttp_access allow authenticated\nhttp_access deny all\n",
            SemanticBlockerKind::ProxyAuthentication,
        ),
    ] {
        let path = directory.path().join(name);
        fs::write(&path, source).expect("Squid fixture");
        let report = import(&path);
        assert!(report.config.is_none(), "{name} unexpectedly finalized");
        assert!(report.has_errors(), "{name} lacks a blocking diagnostic");
        assert!(
            report
                .blocked_capabilities
                .iter()
                .any(|capability| capability.kind == expected),
            "{name} lacks {expected:?}"
        );
    }
}

#[test]
fn canonical_validation_failures_remain_blocking_diagnostics() {
    let directory = tempdir().expect("fixture directory");
    let path = directory.path().join("too-many-connect-ports.conf");
    let ports = (1_u16..=65)
        .map(|port| port.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    fs::write(
        &path,
        format!(
            "http_port 3128\naccess_log none\nforwarded_for delete\nvia off\nacl ssl_ports port {ports}\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\n"
        ),
    )
    .expect("Squid fixture");

    let report = import(&path);
    assert!(report.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Validate
            && diagnostic.code() == oxiroute_import::E_SEMANTICS_NOT_REPRESENTABLE
    }));
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.stage() == DiagnosticStage::Validate)
        .expect("validation blocker");
    assert_eq!(
        diagnostic
            .primary_span()
            .expect("validation origin")
            .source(),
        SourceId::new(0)
    );
}

#[test]
fn hostrouter_sanitized_shape_classifies_every_reachable_directive() {
    let report = import(&fixture("hostrouter-sanitized.conf"));

    assert_eq!(report.source_graph.sources.len(), 1);
    assert!(report.source_graph.includes.is_empty());
    assert!(report.source_graph.snapshot_stable);
    assert_eq!(report.source_graph.expanded_directives.len(), 35);
    assert_eq!(report.decision_ledger.decisions.len(), 35);
    let matrix = hostrouter_behavior_matrix();
    assert_eq!(matrix.len(), 35);
    for (ordinal, (decision, expected)) in report
        .decision_ledger
        .decisions
        .iter()
        .zip(&matrix)
        .enumerate()
    {
        assert_eq!(decision.origin.occurrence.get(), ordinal);
        let DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            activation,
        } = decision.outcome;
        assert_eq!(semantics, expected.0, "ordinal {ordinal}");
        assert_eq!(resolution, expected.1, "ordinal {ordinal}");
        assert_eq!(activation, expected.2, "ordinal {ordinal}");
        assert_ne!(family, DirectiveFamily::Unknown);
        let expanded = &report.source_graph.expanded_directives[ordinal];
        assert_eq!(decision.origin.directive_span, expanded.directive.span);
        assert_eq!(decision.origin.name_span, expanded.directive.name.span);
        assert_eq!(
            decision.origin.argument_spans,
            expanded
                .directive
                .arguments
                .iter()
                .map(|word| word.span)
                .collect::<Vec<_>>()
        );
        assert_eq!(decision.origin.provenance, expanded.provenance);
    }
}

#[test]
fn hostrouter_typed_ir_resolves_acl_auth_port_refresh_and_process_semantics() {
    let report = import(&fixture("hostrouter-sanitized.conf"));
    assert_hostrouter_acls_and_access(&report);
    assert_hostrouter_service_settings(&report);
    assert_hostrouter_authentication(&report);
}

fn assert_hostrouter_acls_and_access(report: &oxiroute_import::squid::ImportReport) {
    assert_eq!(report.effective.acl_definitions.len(), 14);
    assert_eq!(report.effective.acls.len(), 4);
    assert_eq!(report.effective.acls[0].acl_type, AclType::Source);
    assert_eq!(report.effective.acls[0].definitions.len(), 2);
    assert_eq!(report.effective.acls[0].matchers.len(), 2);
    assert_eq!(report.effective.acls[2].acl_type, AclType::Port);
    assert_eq!(report.effective.acls[2].definitions.len(), 10);
    assert_eq!(report.effective.access_rules.len(), 9);
    let policy = &report.effective.access_policies[0];
    assert_eq!(policy.rules.len(), 9);
    assert_eq!(policy.default_action, AccessAction::Allow);
    assert!(
        policy
            .rules
            .iter()
            .flat_map(|rule| &rule.terms)
            .all(|term| !matches!(term.resolution, AclReferenceResolution::Unresolved))
    );
    assert_eq!(
        policy.rules[1].terms[0].resolution,
        AclReferenceResolution::Builtin(BuiltinAcl::Connect)
    );
}

fn assert_hostrouter_service_settings(report: &oxiroute_import::squid::ImportReport) {
    assert_eq!(
        report.effective.ports[0].endpoint,
        PortEndpoint::Wildcard { port: 31280 }
    );
    assert!(report.effective.ports[0].options.is_empty());
    assert_eq!(report.effective.refresh_policy.patterns.len(), 3);
    assert_eq!(
        report.effective.refresh_policy.patterns[0].minimum,
        Duration::from_secs(60 * 60)
    );
    assert_eq!(report.effective.refresh_policy.patterns[0].percent, 20);
    assert_eq!(
        report.effective.logging[0].destination,
        LogDestination::Disabled
    );
    assert_eq!(
        report.effective.dns_nameservers[0].addresses,
        [
            "192.0.2.20".parse::<IpAddr>().expect("synthetic IP"),
            "192.0.2.21".parse::<IpAddr>().expect("synthetic IP")
        ]
    );
    assert!(matches!(
        report.effective.privacy[0],
        PrivacyDirective::ForwardedFor {
            mode: ForwardedForMode::Delete,
            ..
        }
    ));
    assert!(matches!(
        report.effective.privacy[1],
        PrivacyDirective::Via { enabled: false, .. }
    ));
}

fn assert_hostrouter_authentication(report: &oxiroute_import::squid::ImportReport) {
    assert_eq!(report.effective.authentication.len(), 3);
    assert_eq!(report.effective.authentication_schemes.len(), 1);
    assert_eq!(
        report.effective.authentication_schemes[0].credential_ttl,
        Some(Duration::from_secs(2 * 60 * 60))
    );
    assert!(matches!(
        report.effective.authentication[0].value,
        AuthenticationValue::Helper(secret) if secret.kind == SecretKind::AuthenticationHelper
    ));
    assert!(matches!(
        report.effective.authentication[1].value,
        AuthenticationValue::Realm(secret) if secret.kind == SecretKind::AuthenticationRealm
    ));
    let typed_facts = format!(
        "{:?}{:?}",
        report.effective.authentication[0], report.effective.authentication[1]
    );
    assert!(!typed_facts.contains("basic_ncsa_auth"));
    assert!(!typed_facts.contains("Synthetic proxy"));
}

#[test]
fn hostrouter_report_lowers_to_a_complete_forward_proxy_candidate() {
    let report = import(&fixture("hostrouter-sanitized.conf"));
    let config = report.config.as_ref().expect("finalized Squid candidate");
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.forward_proxy_services.len(), 1);
    assert_eq!(
        config.forward_proxy_services[0].connect.allowed_ports,
        [31_000]
    );
    assert!(config.forward_proxy_services[0].access_policy.is_some());
    assert!(matches!(
        config.forward_proxy_services[0].auth.as_ref(),
        Some(oxiroute_config::ForwardProxyAuth::BasicHtpasswdFile {
            username_case_sensitive: false,
            ..
        })
    ));
    assert_squid_provenance_paths_are_unique(&report);
    let paths = report
        .canonical_provenance
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<HashSet<_>>();
    for path in [
        "/listeners",
        "/forward_proxy_services/0",
        "/forward_proxy_services/0/enabled_versions/0",
        "/forward_proxy_services/0/connect/allowed_ports/0",
        "/forward_proxy_services/0/auth/htpasswd_file_path",
        "/forward_proxy_services/0/resolver/nameservers/0",
        "/forward_proxy_services/0/access_policy/rules/0/conditions/0/type",
        "/forward_proxy_services/0/access_policy/rules/1/conditions/1/ranges/0/start",
    ] {
        assert!(paths.contains(path), "missing Squid provenance path {path}");
    }
    assert!(!paths.contains("/listeners/0/tls_profile"));
    assert!(!paths.contains("/listeners/0/downstream_timeouts/keepalive_timeout_ms"));
    assert!(!paths.contains("/forward_proxy_services/0/auth/username_case_sensitive"));
    assert_squid_origin(
        &report,
        "/listeners/0",
        &fixture("hostrouter-sanitized.conf"),
        4,
        0,
    );
    assert_squid_origin(
        &report,
        "/forward_proxy_services/0/auth",
        &fixture("hostrouter-sanitized.conf"),
        27,
        0,
    );
    assert_squid_origin(
        &report,
        "/forward_proxy_services/0/access_policy/rules/0",
        &fixture("hostrouter-sanitized.conf"),
        20,
        0,
    );
    assert!(hostrouter_blockers(&report).is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(
        report.diagnostics[0].severity(),
        oxiroute_import::Severity::Warning
    );
    assert_eq!(report.diagnostics[0].code(), E_UNSUPPORTED_FEATURE);
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code(),
            E_UNKNOWN_DIRECTIVE | E_UNCONSUMED_DIRECTIVE | E_UNSUPPORTED_FORM
        )
    }));
}

#[test]
fn canonical_provenance_retains_include_file_and_stack_for_finalized_fields() {
    let directory = tempdir().expect("Squid fixture directory");
    let included = directory.path().join("forward.conf");
    fs::write(
        &included,
        b"http_port 3128\n\
          access_log none\n\
          forwarded_for delete\n\
          via off\n\
          acl ssl_ports port 443\n\
          http_access deny CONNECT !ssl_ports\n\
          http_access allow all\n",
    )
    .expect("included source");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include forward.conf\n").expect("root source");

    let report = import(&root);
    assert!(report.config.is_some(), "{:#?}", report.diagnostics);
    assert_eq!(report.source_graph.sources.len(), 2);
    assert!(report.source_graph.snapshot_stable);
    assert_squid_provenance_paths_are_unique(&report);
    assert_squid_origin(&report, "/listeners/0", &included, 1, 1);
    assert_squid_origin(
        &report,
        "/forward_proxy_services/0/access_policy/rules/0",
        &included,
        6,
        1,
    );
}

#[test]
fn canonical_provenance_retains_glob_file_and_stack_for_finalized_fields() {
    let directory = tempdir().expect("Squid fixture directory");
    let includes = directory.path().join("conf.d");
    fs::create_dir(&includes).expect("include directory");
    let base = includes.join("10-base.conf");
    fs::write(
        &base,
        b"http_port 3128\n\
          access_log none\n\
          forwarded_for delete\n\
          via off\n\
          acl ssl_ports port 443\n\
          http_access deny CONNECT !ssl_ports\n",
    )
    .expect("base glob source");
    let policy = includes.join("20-policy.conf");
    fs::write(&policy, b"http_access allow all\n").expect("policy glob source");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include conf.d/*.conf\n").expect("glob root source");

    let report = import(&root);
    assert!(report.config.is_some(), "{:#?}", report.diagnostics);
    assert_eq!(report.source_graph.sources.len(), 3);
    assert_eq!(report.source_graph.includes[0].targets.len(), 2);
    assert_squid_provenance_paths_are_unique(&report);
    assert_squid_origin(&report, "/listeners/0", &base, 1, 1);
    assert_squid_origin(
        &report,
        "/forward_proxy_services/0/access_policy/rules/1",
        &policy,
        1,
        1,
    );
}

#[test]
fn unsupported_included_behavior_blocks_with_include_provenance() {
    let directory = tempdir().expect("Squid fixture directory");
    let included = directory.path().join("unsupported.conf");
    fs::write(&included, b"http_port 3128 intercept\n").expect("included source");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include unsupported.conf\n").expect("root source");

    let report = import(&root);
    assert!(report.config.is_none());
    assert!(
        report
            .blocked_capabilities
            .iter()
            .any(|blocked| blocked.kind == SemanticBlockerKind::UnsupportedPortOption)
    );
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == E_UNSUPPORTED_FEATURE)
        .expect("blocking lower diagnostic");
    let primary = diagnostic.primary_span().expect("blocker primary span");
    assert_eq!(primary.source(), SourceId::new(1));
    assert_eq!(diagnostic.include_stack().len(), 1);
    assert_eq!(diagnostic.include_stack()[0].source(), SourceId::new(0));
}

#[test]
fn selected_native_root_report_retains_canonical_provenance() {
    let directory = tempdir().expect("Squid fixture directory");
    let root = directory.path().join("active.conf");
    fs::write(
        &root,
        b"http_port 3128\n\
          access_log none\n\
          forwarded_for delete\n\
          via off\n\
          acl ssl_ports port 443\n\
          http_access deny CONNECT !ssl_ports\n\
          http_access allow all\n",
    )
    .expect("active source");

    let report = import_selected(
        &[OsString::from("-f"), root.clone().into_os_string()],
        Path::new("/synthetic/default.conf"),
    );
    assert_eq!(report.selection.active_root, root);
    assert!(report.import.config.is_some());
    assert_squid_provenance_paths_are_unique(&report.import);
    assert_squid_origin(&report.import, "/listeners/0", &root, 1, 0);
}

#[allow(clippy::naive_bytecount)]
fn assert_squid_provenance_paths_are_unique(report: &oxiroute_import::squid::ImportReport) {
    let mut paths = HashSet::new();
    assert!(!report.canonical_provenance.is_empty());
    for entry in &report.canonical_provenance {
        assert!(
            paths.insert(entry.path.as_str()),
            "duplicate path {}",
            entry.path
        );
        assert!(!entry.origins.is_empty(), "{} lacks origins", entry.path);
        for origin in &entry.origins {
            let source = report
                .source_graph
                .source(origin.provenance.source)
                .expect("canonical provenance source");
            assert_eq!(origin.directive_span.source(), origin.provenance.source);
            assert!(source.source.path().is_some());
            assert!(
                source.source.bytes()[..origin.directive_span.range().start()]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count()
                    < source.source.len()
            );
        }
    }
}

#[allow(clippy::naive_bytecount)]
fn assert_squid_origin(
    report: &oxiroute_import::squid::ImportReport,
    path: &str,
    expected_source: &Path,
    expected_line: usize,
    expected_include_depth: usize,
) {
    let entry = report
        .canonical_provenance
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("missing Squid provenance {path}"));
    let expected_source = fs::canonicalize(expected_source).expect("canonical source path");
    assert!(entry.origins.iter().any(|origin| {
        let source = report
            .source_graph
            .source(origin.provenance.source)
            .expect("origin source");
        let line = source.source.bytes()[..origin.directive_span.range().start()]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1;
        source.canonical_path == expected_source
            && line == expected_line
            && origin.provenance.include_stack.len() == expected_include_depth
    }));
}

fn hostrouter_blockers(
    report: &oxiroute_import::squid::ImportReport,
) -> Vec<(SemanticBlockerKind, usize)> {
    report
        .blocked_capabilities
        .iter()
        .map(|blocker| (blocker.kind, blocker.occurrences.len()))
        .collect()
}

fn hostrouter_behavior_matrix() -> Vec<(DirectiveSemantics, DirectiveResolution, Activation)> {
    let blocked = Activation::Blocked;
    let mut matrix = vec![
        (
            DirectiveSemantics::HttpPort,
            DirectiveResolution::Append,
            blocked(SemanticBlockerKind::ForwardProxyListener),
        ),
        (
            DirectiveSemantics::AccessLogging,
            DirectiveResolution::Append,
            blocked(SemanticBlockerKind::AccessLoggingPolicy),
        ),
        (
            DirectiveSemantics::DnsNameservers,
            DirectiveResolution::Append,
            blocked(SemanticBlockerKind::ResolverPolicy),
        ),
    ];
    matrix.extend((0..2).map(|_| {
        (
            DirectiveSemantics::AclSource,
            DirectiveResolution::MergeSameName,
            blocked(SemanticBlockerKind::SourceAddressAcl),
        )
    }));
    matrix.extend((0..11).map(|_| {
        (
            DirectiveSemantics::AclPort,
            DirectiveResolution::MergeSameName,
            blocked(SemanticBlockerKind::DestinationPortAcl),
        )
    }));
    matrix.extend((0..7).map(|_| {
        (
            DirectiveSemantics::HttpAccess,
            DirectiveResolution::OrderedFirstMatch,
            blocked(SemanticBlockerKind::OrderedHttpAccess),
        )
    }));
    matrix.extend([
        (
            DirectiveSemantics::AuthenticationHelper,
            DirectiveResolution::LastWins,
            blocked(SemanticBlockerKind::ProxyAuthentication),
        ),
        (
            DirectiveSemantics::AuthenticationRealm,
            DirectiveResolution::LastWins,
            blocked(SemanticBlockerKind::ProxyAuthentication),
        ),
        (
            DirectiveSemantics::AuthenticationCredentialTtl,
            DirectiveResolution::LastWins,
            blocked(SemanticBlockerKind::ProxyAuthentication),
        ),
        (
            DirectiveSemantics::AclProxyAuth,
            DirectiveResolution::MergeSameName,
            blocked(SemanticBlockerKind::ProxyAuthenticationAcl),
        ),
    ]);
    matrix.extend((0..2).map(|_| {
        (
            DirectiveSemantics::HttpAccess,
            DirectiveResolution::OrderedFirstMatch,
            blocked(SemanticBlockerKind::OrderedHttpAccess),
        )
    }));
    matrix.extend([
        (
            DirectiveSemantics::CoreDumpDirectory,
            DirectiveResolution::Externalized,
            Activation::Externalized,
        ),
        (
            DirectiveSemantics::ForwardedFor,
            DirectiveResolution::LastWins,
            blocked(SemanticBlockerKind::ForwardedForPolicy),
        ),
        (
            DirectiveSemantics::Via,
            DirectiveResolution::LastWins,
            blocked(SemanticBlockerKind::ViaPolicy),
        ),
    ]);
    matrix.extend((0..3).map(|_| {
        (
            DirectiveSemantics::RefreshPattern,
            DirectiveResolution::Externalized,
            Activation::Externalized,
        )
    }));
    matrix
}

#[test]
fn hostrouter_http_access_uses_ordered_first_match_and_native_default() {
    let report = import(&fixture("hostrouter-sanitized.conf"));
    let policy = &report.effective.access_policies[0];

    let authenticated = policy.evaluate(|term| match term.name.value.as_slice() {
        b"acl_2" | b"acl_8" | b"all" => Some(true),
        b"CONNECT" | b"localhost" | b"manager" | b"to_localhost" | b"to_linklocal" => Some(false),
        _ => None,
    });
    assert_eq!(
        authenticated,
        AccessEvaluation::Decided {
            action: AccessAction::Allow,
            matched_rule: Some(report.effective.access_rules[7].origin.occurrence),
        }
    );

    let unauthenticated = policy.evaluate(|term| match term.name.value.as_slice() {
        b"acl_2" | b"all" => Some(true),
        b"acl_8" | b"CONNECT" | b"localhost" | b"manager" | b"to_localhost" | b"to_linklocal" => {
            Some(false)
        }
        _ => None,
    });
    assert_eq!(
        unauthenticated,
        AccessEvaluation::Decided {
            action: AccessAction::Deny,
            matched_rule: Some(report.effective.access_rules[8].origin.occurrence),
        }
    );
}

#[test]
fn include_graph_expands_globs_in_byte_sorted_parse_order_with_provenance() {
    let directory = tempdir().expect("temp directory");
    let includes = directory.path().join("conf.d");
    fs::create_dir(&includes).expect("include directory");
    fs::write(
        includes.join("20-second.conf"),
        b"acl second src 192.0.2.2\n",
    )
    .expect("second include");
    fs::write(includes.join("10-first.conf"), b"acl first src 192.0.2.1\n").expect("first include");
    fs::write(includes.join(".hidden.conf"), b"unknown hidden\n").expect("hidden include");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"acl root src 192.0.2.10\ninclude conf.d/10-*.conf conf.d/20-*.conf\nhttp_access allow root\n",
    )
    .expect("root source");

    let report = load(&root);
    assert!(report.diagnostics().is_empty());
    let graph = report.value();
    assert_eq!(graph.sources.len(), 3);
    assert_eq!(graph.includes.len(), 1);
    assert_eq!(graph.includes[0].targets.len(), 2);
    assert_eq!(graph.expanded_directives.len(), 5);
    let names = graph
        .expanded_directives
        .iter()
        .map(|expanded| expanded.directive.name.value.as_slice())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            b"acl".as_slice(),
            b"include".as_slice(),
            b"acl".as_slice(),
            b"acl".as_slice(),
            b"http_access".as_slice(),
        ]
    );
    let acl_names = graph
        .expanded_directives
        .iter()
        .filter(|expanded| expanded.directive.name.value == b"acl")
        .map(|expanded| expanded.directive.arguments[0].value.as_slice())
        .collect::<Vec<_>>();
    assert_eq!(
        acl_names,
        [
            b"root".as_slice(),
            b"first".as_slice(),
            b"second".as_slice()
        ]
    );
    assert!(graph.expanded_directives[2].provenance.include_stack.len() == 1);
    assert!(graph.expanded_directives[3].provenance.include_stack.len() == 1);
    let analyzed = oxiroute_import::squid::analyze(graph);
    assert!(analyzed.diagnostics().is_empty());
    assert_eq!(
        analyzed.value().acl_definitions[1]
            .origin
            .provenance
            .include_stack
            .len(),
        1
    );
}

#[test]
fn pipe_backed_includes_are_retained_but_never_executed() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include |synthetic-generator\n").expect("root source");

    let report = load(&root);
    assert_eq!(report.value().expanded_directives.len(), 1);
    assert_eq!(report.value().includes.len(), 1);
    assert_eq!(report.value().includes[0].targets.len(), 1);
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == E_UNSUPPORTED_FEATURE)
    );
    let imported = import(&root);
    assert!(matches!(
        imported.decision_ledger.decisions[0].outcome,
        DecisionOutcome::Classified {
            resolution: DirectiveResolution::Blocked,
            activation: Activation::Blocked(SemanticBlockerKind::IncludeExpansion),
            ..
        }
    ));
}

#[test]
fn include_cycle_is_retained_without_recursive_expansion() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include squid.conf\n").expect("root source");

    let report = load(&root);
    assert_eq!(report.value().expanded_directives.len(), 1);
    assert_eq!(report.value().includes.len(), 1);
    assert!(report.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_INCLUDE_CYCLE && diagnostic.stage() == DiagnosticStage::Source
    }));
}

#[test]
fn lexer_preserves_bytes_while_parser_decodes_quotes_comments_and_continuations() {
    let bytes = b"acl local src 192.0.2.0/24 \\\r\n  198.51.100.0/24 # synthetic\r\ninclude \"conf.d/with space.conf\"\n";
    let source = SourceFile::new(SourceId::new(7), "squid.conf", bytes.as_slice());

    let lexed = lex(&source);
    assert!(lexed.diagnostics().is_empty());
    assert_eq!(lexed.value().len(), 2);
    assert_eq!(lexed.value()[0].words.len(), 5);
    assert_eq!(lexed.value()[0].comments.len(), 1);
    assert_eq!(source.bytes(), bytes);

    let parsed = parse(&source);
    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.value().directives.len(), 2);
    assert_eq!(
        parsed.value().directives[1].arguments[0].value,
        b"conf.d/with space.conf"
    );
}

#[test]
fn cache_peer_credentials_become_typed_secret_facts() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"cache_peer peer.example.test parent 3128 0 login=synthetic-user:synthetic-password token=synthetic-token no-query\n",
    )
    .expect("root source");

    let report = import(&root);
    let peer = &report.effective.cache_peers[0];
    assert!(matches!(
        peer.options[0],
        PeerOption::Secret(secret) if secret.kind == SecretKind::PeerCredentials
    ));
    assert!(matches!(
        peer.options[1],
        PeerOption::Secret(secret) if secret.kind == SecretKind::BearerToken
    ));
    assert!(matches!(peer.options[2], PeerOption::NoQuery));
    let typed_peer = format!("{peer:?}");
    assert!(!typed_peer.contains("synthetic-password"));
    assert!(!typed_peer.contains("synthetic-token"));
    assert!(
        report
            .blocked_capabilities
            .iter()
            .any(|blocked| blocked.kind == SemanticBlockerKind::CachePeerHierarchy)
    );
}

#[test]
fn static_parent_peers_and_global_never_direct_lower_in_source_order() {
    let directory = tempdir().expect("fixture directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"http_port 3128\n\
          access_log none\n\
          forwarded_for delete\n\
          via off\n\
          acl ssl_ports port 443\n\
          http_access deny CONNECT !ssl_ports\n\
          http_access allow all\n\
          cache_peer first.example.test parent 3128 0\n\
          cache_peer 192.0.2.44 parent 8080 0\n\
          never_direct allow all\n",
    )
    .expect("static peer source");

    let report = import(&root);
    let config = report.config.as_ref().expect("static peer candidate");
    let policy = &config.forward_proxy_services[0].peer_policy;
    assert_eq!(policy.peers.len(), 2);
    assert_eq!(policy.peers[0].host, "first.example.test");
    assert_eq!(policy.peers[0].port, 3128);
    assert_eq!(policy.peers[1].host, "192.0.2.44");
    assert_eq!(policy.peers[1].port, 8080);
    assert_eq!(
        policy.direct_fallback,
        oxiroute_config::ForwardDirectFallback::Denied
    );
    assert_eq!(policy.max_retries, 1);
    assert!(report.blocked_capabilities.is_empty());
    assert!(
        report
            .canonical_provenance
            .iter()
            .any(|entry| { entry.path == "/forward_proxy_services/0/peer_policy/peers/0/host" })
    );
    assert!(
        report
            .canonical_provenance
            .iter()
            .any(|entry| { entry.path == "/forward_proxy_services/0/peer_policy/direct_fallback" })
    );
    assert!(report.decision_ledger.decisions.iter().any(|decision| {
        decision.name == b"cache_peer"
            && matches!(
                decision.outcome,
                DecisionOutcome::Classified {
                    activation: Activation::Structural,
                    ..
                }
            )
    }));
}

#[test]
fn global_always_direct_lowers_to_required_direct_fallback() {
    let directory = tempdir().expect("fixture directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"http_port 3128\naccess_log none\nforwarded_for delete\nvia off\nacl ssl_ports port 443\nhttp_access deny CONNECT !ssl_ports\nhttp_access allow all\nalways_direct allow all\n",
    )
    .expect("direct source");

    let report = import(&root);
    let config = report.config.as_ref().expect("direct candidate");
    assert_eq!(
        config.forward_proxy_services[0].peer_policy.direct_fallback,
        oxiroute_config::ForwardDirectFallback::Required
    );
    assert!(
        config.forward_proxy_services[0]
            .peer_policy
            .peers
            .is_empty()
    );
}

#[test]
fn unsupported_peer_and_direct_forms_keep_stable_blockers() {
    let directory = tempdir().expect("fixture directory");
    let base =
        "http_port 3128\naccess_log none\nforwarded_for delete\nvia off\nhttp_access allow all\n";
    for (name, extra, expected_message) in [
        (
            "sibling",
            "cache_peer peer.example.test sibling 3128 0\n",
            "Squid cache_peer must be an ordered static parent with HTTP port, ICP port 0, and no options",
        ),
        (
            "option",
            "cache_peer peer.example.test parent 3128 0 no-query\n",
            "Squid cache_peer must be an ordered static parent with HTTP port, ICP port 0, and no options",
        ),
        (
            "icp",
            "cache_peer peer.example.test parent 3128 3130\n",
            "Squid cache_peer must be an ordered static parent with HTTP port, ICP port 0, and no options",
        ),
        (
            "conditional-direct",
            "acl target dstdomain example.test\nalways_direct allow target\n",
            "Squid direct-routing form is outside the exact global direct-fallback subset",
        ),
    ] {
        let root = directory.path().join(format!("{name}.conf"));
        fs::write(&root, format!("{base}{extra}")).expect("unsupported source");
        let report = import(&root);
        assert!(report.config.is_none(), "{name} unexpectedly lowered");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message() == expected_message }),
            "{name} lacks stable blocker message"
        );
    }
}

#[test]
fn source_loading_stops_before_retaining_an_oversized_file() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"http_port 31280\n").expect("root source");
    let limits = SquidLoadLimits {
        max_source_bytes: 8,
        ..SquidLoadLimits::default()
    };

    let report = load_with_limits(&root, limits);
    assert!(report.value().root.is_none());
    assert!(report.value().sources.is_empty());
    assert!(report.value().expanded_directives.is_empty());
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == oxiroute_import::E_SOURCE_LIMIT)
    );
}

#[test]
fn cache_policy_and_storage_are_classified_without_placeholders() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"cache_mem 16 MB\ncache_dir ufs /tmp/squid-synthetic-cache 16 16 256\n",
    )
    .expect("root source");

    let report = import(&root);
    assert_eq!(report.effective.cache_policy.len(), 1);
    assert_eq!(report.effective.storage.len(), 1);
    assert!(report.draft.listeners.is_empty());
    assert!(report.config.is_none());
    for kind in [
        SemanticBlockerKind::ForwardProxyListener,
        SemanticBlockerKind::CachePolicy,
        SemanticBlockerKind::StoragePolicy,
    ] {
        assert!(
            report
                .blocked_capabilities
                .iter()
                .any(|blocked| blocked.kind == kind)
        );
    }
}

#[test]
fn native_cli_root_discovery_uses_the_last_f_argument() {
    let arguments = [
        OsString::from("--foreground"),
        OsString::from("-f"),
        OsString::from("/tmp/synthetic-first.conf"),
        OsString::from("-f/tmp/synthetic-active.conf"),
        OsString::from("-sYC"),
    ];
    let report = discover_root(&arguments, std::path::Path::new("/etc/squid/squid.conf"));
    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().command_line_roots.len(), 2);
    assert_eq!(
        report.value().active_root,
        std::path::Path::new("/tmp/synthetic-active.conf")
    );
    assert_eq!(
        report.value().source,
        RootSelectionSource::CommandLine { argument_index: 3 }
    );

    let default = discover_root(
        &[OsString::from("--foreground"), OsString::from("-sYC")],
        std::path::Path::new("/etc/squid/squid.conf"),
    );
    assert_eq!(default.value().source, RootSelectionSource::CompiledDefault);
}

#[test]
fn native_cli_import_preserves_selected_root_and_semantic_report() {
    let directory = tempdir().expect("temp directory");
    let first = directory.path().join("first.conf");
    let active = directory.path().join("active.conf");
    fs::write(&first, b"via on\n").expect("first source");
    fs::write(&active, b"via off\n").expect("active source");
    let arguments = [
        OsString::from("-f"),
        first.into_os_string(),
        OsString::from("-f"),
        active.clone().into_os_string(),
    ];

    let report = import_selected(&arguments, std::path::Path::new("/synthetic/default.conf"));
    assert_eq!(report.selection.active_root, active);
    assert_eq!(report.import.decision_ledger.decisions.len(), 1);
    assert!(matches!(
        report.import.effective.privacy[0],
        PrivacyDirective::Via { enabled: false, .. }
    ));
}

#[test]
fn lowering_adapter_receives_typed_ir_without_a_duplicate_schema() {
    struct CountAdapter;

    impl SquidLoweringAdapter for CountAdapter {
        type Output = (usize, usize, usize);
        type Error = std::convert::Infallible;

        fn lower(
            &self,
            source: oxiroute_import::squid::LoweringView<'_>,
        ) -> Result<Self::Output, Self::Error> {
            Ok((
                source.effective.ports.len(),
                source.decision_ledger.decisions.len(),
                source.blocked_capabilities.len(),
            ))
        }
    }

    let report = import(&fixture("hostrouter-sanitized.conf"));
    assert_eq!(report.lower_with(&CountAdapter), Ok((1, 35, 0)));
}

#[test]
fn header_privacy_dns_logging_and_port_options_have_typed_semantics() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"acl clients src 192.0.2.0/24\n\
          http_port 192.0.2.10:3128 intercept name=synthetic\n\
          request_header_access X-Synthetic deny clients\n\
          request_header_replace X-Synthetic synthetic-value\n\
          dns_nameservers 192.0.2.53\n\
          access_log stdio:/tmp/synthetic-access.log squid\n\
          forwarded_for truncate\n\
          via on\n",
    )
    .expect("synthetic source");

    let report = import(&root);
    assert_eq!(report.effective.ports.len(), 1);
    assert_eq!(report.effective.ports[0].options.len(), 2);
    assert_eq!(report.effective.access_policies.len(), 1);
    assert_eq!(
        report.effective.access_policies[0].rules[0]
            .selector
            .as_ref()
            .expect("header selector")
            .value,
        b"X-Synthetic"
    );
    assert!(matches!(
        report.effective.privacy[0],
        PrivacyDirective::HeaderReplace { request: true, .. }
    ));
    assert!(matches!(
        report.effective.logging[0].destination,
        LogDestination::Stdio(_)
    ));
    assert_eq!(report.effective.dns_nameservers[0].addresses.len(), 1);
    assert!(report.decision_ledger.decisions.iter().all(|decision| {
        !matches!(
            decision.outcome,
            DecisionOutcome::Classified {
                semantics: DirectiveSemantics::Unknown,
                ..
            }
        )
    }));
}

#[test]
fn duplicate_acl_and_authentication_resolution_rules_are_explicit() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"acl mixed src 192.0.2.0/24\n\
          acl mixed port 3128\n\
          auth_param basic realm First synthetic realm\n\
          auth_param basic realm Last synthetic realm\n\
          http_access deny mixed\n",
    )
    .expect("synthetic source");

    let report = import(&root);
    assert_eq!(report.effective.acls.len(), 1);
    assert_eq!(report.effective.acls[0].acl_type, AclType::Source);
    let second = &report.decision_ledger.decisions[1];
    assert!(matches!(
        second.outcome,
        DecisionOutcome::Classified {
            resolution: DirectiveResolution::MergeSameName,
            activation: Activation::Blocked(SemanticBlockerKind::ConflictingAclType),
            ..
        }
    ));
    let scheme = &report.effective.authentication_schemes[0];
    assert_eq!(scheme.parameters.len(), 2);
    assert_eq!(
        scheme.realm.expect("last realm fact").span,
        match report.effective.authentication[1].value {
            AuthenticationValue::Realm(secret) => secret.span,
            _ => panic!("expected redacted realm"),
        }
    );
}

#[test]
fn includes_are_transparent_to_native_last_wins_settings() {
    let directory = tempdir().expect("temp directory");
    let included = directory.path().join("included.conf");
    fs::write(
        &included,
        b"auth_param basic realm Included synthetic realm\n",
    )
    .expect("included source");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"auth_param basic realm Before synthetic realm\n\
          include included.conf\n\
          auth_param basic realm After synthetic realm\n",
    )
    .expect("root source");

    let report = import(&root);
    let scheme = &report.effective.authentication_schemes[0];
    assert_eq!(scheme.parameters.len(), 3);
    let final_parameter = report
        .effective
        .authentication
        .last()
        .expect("final parameter");
    assert_eq!(
        scheme.realm.expect("effective realm").span,
        match final_parameter.value {
            AuthenticationValue::Realm(secret) => secret.span,
            _ => panic!("expected redacted realm"),
        }
    );
    assert!(final_parameter.origin.provenance.include_stack.is_empty());
    assert_eq!(
        report.effective.authentication[1]
            .origin
            .provenance
            .include_stack
            .len(),
        1
    );
}
