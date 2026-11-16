use std::{collections::HashSet, fs, net::IpAddr, time::Duration};

use oxiroute_import::{
    DiagnosticStage, E_UNSUPPORTED_FEATURE, SourceId,
    squid::{
        AccessAction, AccessEvaluation, AclReferenceResolution, AclType, Activation,
        AuthenticationValue, BuiltinAcl, DecisionOutcome, DirectiveFamily, DirectiveResolution,
        DirectiveSemantics, E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNSUPPORTED_FORM,
        ForwardedForMode, LogDestination, PortEndpoint, PrivacyDirective, SecretKind,
        SemanticBlockerKind, import,
    },
};
use tempfile::tempdir;

use super::common::{assert_squid_origin, assert_squid_provenance_paths_are_unique, fixture};

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
        assert!(
            report
                .candidate
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "{name} unexpectedly finalized"
        );
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
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
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
fn forward_proxy_sanitized_shape_classifies_every_reachable_directive() {
    let report = import(&fixture("forward-proxy-auth.conf"));

    assert_eq!(report.source_graph.sources.len(), 1);
    assert!(report.source_graph.includes.is_empty());
    assert!(report.source_graph.snapshot_stable);
    assert_eq!(report.source_graph.expanded_directives.len(), 35);
    assert_eq!(report.decision_ledger.decisions.len(), 35);
    let matrix = forward_proxy_behavior_matrix();
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
fn forward_proxy_typed_ir_resolves_acl_auth_port_refresh_and_process_semantics() {
    let report = import(&fixture("forward-proxy-auth.conf"));
    assert_forward_proxy_acls_and_access(&report);
    assert_forward_proxy_service_settings(&report);
    assert_forward_proxy_authentication(&report);
}

fn assert_forward_proxy_acls_and_access(report: &oxiroute_import::squid::ImportReport) {
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

fn assert_forward_proxy_service_settings(report: &oxiroute_import::squid::ImportReport) {
    assert_eq!(
        report.effective.ports[0].endpoint,
        PortEndpoint::Wildcard { port: 31280 }
    );
    assert!(report.effective.ports[0].options.is_empty());
    assert_eq!(report.effective.refresh_policy.patterns.len(), 3);
    assert_eq!(
        report.effective.refresh_policy.patterns[0].minimum,
        Duration::from_hours(1)
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

fn assert_forward_proxy_authentication(report: &oxiroute_import::squid::ImportReport) {
    assert_eq!(report.effective.authentication.len(), 3);
    assert_eq!(report.effective.authentication_schemes.len(), 1);
    assert_eq!(
        report.effective.authentication_schemes[0].credential_ttl,
        Some(Duration::from_hours(2))
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
fn forward_proxy_report_lowers_to_a_complete_forward_proxy_candidate() {
    let report = import(&fixture("forward-proxy-auth.conf"));
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized Squid candidate");
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
        .candidate
        .provenance
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
        &fixture("forward-proxy-auth.conf"),
        4,
        0,
    );
    assert_squid_origin(
        &report,
        "/forward_proxy_services/0/auth",
        &fixture("forward-proxy-auth.conf"),
        27,
        0,
    );
    assert_squid_origin(
        &report,
        "/forward_proxy_services/0/access_policy/rules/0",
        &fixture("forward-proxy-auth.conf"),
        20,
        0,
    );
    assert!(forward_proxy_blockers(&report).is_empty());
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
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some(),
        "{:#?}",
        report.diagnostics
    );
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
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some(),
        "{:#?}",
        report.diagnostics
    );
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

fn forward_proxy_blockers(
    report: &oxiroute_import::squid::ImportReport,
) -> Vec<(SemanticBlockerKind, usize)> {
    report
        .blocked_capabilities
        .iter()
        .map(|blocker| (blocker.kind, blocker.occurrences.len()))
        .collect()
}

fn forward_proxy_behavior_matrix() -> Vec<(DirectiveSemantics, DirectiveResolution, Activation)> {
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
fn forward_proxy_http_access_uses_ordered_first_match_and_native_default() {
    let report = import(&fixture("forward-proxy-auth.conf"));
    let policy = &report.effective.access_policies[0];

    let authenticated = policy.evaluate(|term| match term.name.value.as_slice() {
        b"clients" | b"authenticated" | b"safe_ports" | b"all" => Some(true),
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
        b"clients" | b"safe_ports" | b"all" => Some(true),
        b"authenticated" | b"CONNECT" | b"localhost" | b"manager" | b"to_localhost"
        | b"to_linklocal" => Some(false),
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
