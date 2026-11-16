use std::fs;

use oxiroute_import::{
    DiagnosticStage, E_INCLUDE_CYCLE, E_UNSUPPORTED_FEATURE, SourceId,
    squid::{
        Activation, AuthenticationValue, DecisionOutcome, DirectiveFamily, DirectiveResolution,
        DirectiveSemantics, IncludeTargetStatus, SemanticBlockerKind, SquidLoadLimits, import,
        load, load_with_limits,
    },
};
use tempfile::tempdir;

#[test]
fn unsupported_included_behavior_blocks_with_include_provenance() {
    let directory = tempdir().expect("Squid fixture directory");
    let included = directory.path().join("unsupported.conf");
    fs::write(&included, b"http_port 3128 intercept\n").expect("included source");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include unsupported.conf\n").expect("root source");

    let report = import(&root);
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
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
        b"acl root src 192.0.2.10\ninclude conf.d/*.conf\nhttp_access allow root\n",
    )
    .expect("root source");

    let report = load(&root);
    assert!(report.diagnostics().is_empty());
    let graph = report.value();
    assert_eq!(graph.sources.len(), 3);
    assert_eq!(graph.includes.len(), 1);
    assert_eq!(graph.includes[0].targets.len(), 2);
    assert!(graph.snapshot_stable);
    assert!(matches!(
        graph.includes[0].targets[0].status,
        IncludeTargetStatus::Expanded(source) if source == SourceId::new(1)
    ));
    assert!(matches!(
        graph.includes[0].targets[1].status,
        IncludeTargetStatus::Expanded(source) if source == SourceId::new(2)
    ));
    assert_eq!(
        graph.includes[0].targets[0]
            .canonical_path
            .as_ref()
            .expect("first canonical include"),
        &fs::canonicalize(includes.join("10-first.conf")).expect("first canonical path")
    );
    assert_eq!(
        graph.includes[0].targets[1]
            .canonical_path
            .as_ref()
            .expect("second canonical include"),
        &fs::canonicalize(includes.join("20-second.conf")).expect("second canonical path")
    );
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
    assert_eq!(
        graph.expanded_directives[2].provenance.include_stack.len(),
        1
    );
    assert_eq!(
        graph.expanded_directives[3].provenance.include_stack.len(),
        1
    );
    let analyzed = oxiroute_import::squid::analyze(graph);
    assert!(analyzed.diagnostics().is_empty());
    assert!(matches!(
        analyzed.value().ledger.decisions[1].outcome,
        DecisionOutcome::Classified {
            family: DirectiveFamily::Include,
            semantics: DirectiveSemantics::Include,
            resolution: DirectiveResolution::Structural,
            activation: Activation::Structural,
        }
    ));
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
fn regular_file_include_boundary_rejects_directories_and_missing_paths() {
    let directory = tempdir().expect("temp directory");
    fs::create_dir(directory.path().join("include-dir")).expect("include directory");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include include-dir\ninclude missing.conf\n").expect("root source");

    let loaded = load(&root);
    assert_eq!(loaded.value().sources.len(), 1);
    assert_eq!(loaded.value().includes.len(), 2);
    assert!(loaded.value().includes.iter().all(|edge| {
        edge.targets.is_empty() && edge.failure == Some(oxiroute_import::E_INCLUDE_NOT_FOUND)
    }));
    assert_eq!(
        loaded
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code() == oxiroute_import::E_INCLUDE_NOT_FOUND)
            .count(),
        2
    );

    let imported = import(&root);
    assert!(
        imported
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(imported.blocked_capabilities.iter().any(|blocked| {
        blocked.kind == SemanticBlockerKind::IncludeExpansion
            && blocked.occurrences
                == [
                    oxiroute_import::squid::OccurrenceId::new(0),
                    oxiroute_import::squid::OccurrenceId::new(1),
                ]
    }));
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
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("static peer candidate");
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
            .candidate
            .provenance
            .iter()
            .any(|entry| { entry.path == "/forward_proxy_services/0/peer_policy/peers/0/host" })
    );
    assert!(
        report
            .candidate
            .provenance
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
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("direct candidate");
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
