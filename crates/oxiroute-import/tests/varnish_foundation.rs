#[path = "../src/source.rs"]
mod source;
pub use source::{ByteRange, SourceFile, SourceId, Span};

#[path = "../src/candidate.rs"]
#[allow(dead_code)]
mod candidate;
pub use candidate::{
    CanonicalCandidate, CanonicalDraft, CanonicalProvenance, SourceImportMetadata,
};

#[path = "../src/diagnostic.rs"]
#[allow(dead_code)]
mod diagnostic;
pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INVALID_VALUE,
    E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE, Report, Severity,
};

#[path = "../src/limits.rs"]
#[allow(dead_code)]
mod limits;
pub use limits::{
    E_INCLUDE_CYCLE, E_INCLUDE_NOT_FOUND, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES,
    MAX_GLOB_MATCHES, MAX_INCLUDE_DEPTH, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH,
    MAX_TOKENS_PER_SOURCE,
};

#[path = "../src/varnish/mod.rs"]
#[allow(dead_code, unused_imports)]
mod varnish;

use varnish::{
    BackendKind, BackendProperty, BackendReference, BuiltinComposition, Condition, Declaration,
    FeatureBehavior, FlowAction, HeaderOperation, ImportReport, Invalidation, LoweringBlocker,
    LoweringStatus, MAX_INVOCATION_ARGUMENTS, ParserLimits, ProbeReference, ResponseAction,
    StatementClassification, StatementKind, StorageKind, SubroutineKind, UnsupportedBehavior,
    VarnishLoadLimits, VarnishdInvocation, VclVersion, VersionOrigin, analyze, decision_signatures,
    import, load_with_limits, parse, parse_with_limits,
};

const REPRESENTATIVE: &[u8] = include_bytes!("fixtures/varnish/representative.vcl");
const SHARED_BACKENDS: &[u8] = include_bytes!("fixtures/varnish/shared-backends.vcl");
const EXPECTED_DECISIONS: &str = include_str!("fixtures/varnish/representative.decisions");
const EXPECTED_V41_DECISIONS: &str = include_str!("fixtures/varnish/v41/expected.decisions");

#[test]
fn parses_bounded_sources_and_retains_only_the_bounded_prefix() {
    let source = source(
        1,
        "bounded.vcl",
        b"vcl 4.1; sub vcl_recv { return (hash); }",
    );
    let report = parse_with_limits(
        &source,
        ParserLimits {
            source_bytes: 8,
            ..ParserLimits::default()
        },
    );

    assert_eq!(report.value().declarations.len(), 1);
    assert_eq!(report.diagnostics().len(), 1);
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == E_SOURCE_LIMIT)
    );
    assert!(report.value().declarations[0].span().range().end() <= 8);
}

#[test]
fn statement_limit_keeps_a_deterministic_classifiable_prefix() {
    let source = source(
        1,
        "statements.vcl",
        b"sub vcl_recv { return (pass); return (pipe); } sub vcl_hash { return (lookup); }",
    );
    let limits = ParserLimits {
        statements: 1,
        ..ParserLimits::default()
    };
    let first = parse_with_limits(&source, limits);
    let second = parse_with_limits(&source, limits);

    assert_eq!(first, second);
    assert_eq!(first.diagnostics().len(), 1);
    assert_eq!(first.diagnostics()[0].code(), E_SOURCE_LIMIT);
    let retained = first
        .value()
        .declarations
        .iter()
        .filter_map(|declaration| match declaration {
            Declaration::Subroutine(subroutine) => Some(subroutine.statements.len()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retained, [1, 0]);
}

#[test]
fn parses_declarations_subroutines_and_statement_spans_as_bytes() {
    let source = source(1, "representative.vcl", REPRESENTATIVE);
    let report = parse(&source);

    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );
    assert_eq!(report.value().span, source.full_span());
    assert!(matches!(
        report.value().declarations.first(),
        Some(Declaration::Version { .. })
    ));
    let recv = report
        .value()
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Subroutine(subroutine) if subroutine.name.bytes == b"vcl_recv" => {
                Some(subroutine)
            }
            _ => None,
        })
        .expect("vcl_recv declaration");

    assert_eq!(recv.statements.len(), 6);
    assert!(matches!(recv.statements[0].kind, StatementKind::If(_)));
    for statement in &recv.statements {
        assert_eq!(statement.span.source(), source.id());
        assert!(source.slice(statement.span.range()).is_some());
    }
}

#[test]
fn complete_ordered_decision_ledger_matches_the_differential_fixture() {
    let report = representative_report();
    let actual = decision_signatures(&report).join("\n");

    assert_eq!(actual.trim(), EXPECTED_DECISIONS.trim());
    assert_eq!(report.statements.len(), representative_statement_count());
    assert_eq!(
        report.lowering,
        LoweringStatus::Blocked(LoweringBlocker::UnsupportedBehavior)
    );
}

#[test]
fn typed_ir_covers_cache_headers_cookies_backends_and_invalidation() {
    let report = representative_report();

    assert!(
        report.backends[0]
            .properties
            .iter()
            .any(|property| { matches!(property, BackendProperty::Host(_)) })
    );
    assert_eq!(report.directors[0].members.len(), 1);
    assert!(report.statements.iter().any(|decision| matches!(
        decision.classification,
        StatementClassification::BackendSelection(BackendReference::Director {
            declaration: 0,
            ..
        })
    )));
    assert_eq!(
        report
            .subroutines
            .iter()
            .map(|subroutine| subroutine.kind)
            .collect::<Vec<_>>(),
        [
            SubroutineKind::Recv,
            SubroutineKind::Hash,
            SubroutineKind::BackendResponse,
            SubroutineKind::Deliver,
            SubroutineKind::Synth,
        ]
    );
    assert!(report.statements.iter().any(|decision| matches!(
        decision.classification,
        StatementClassification::CacheDecision(FlowAction::Lookup)
    )));
    assert!(report.statements.iter().any(|decision| matches!(
        decision.classification,
        StatementClassification::Invalidation(Invalidation::Ban(_))
    )));
    assert!(report.statements.iter().any(|decision| matches!(
        decision.classification,
        StatementClassification::Response(ResponseAction::Redirect { status: 302, .. })
    )));
    assert!(report.statements.iter().any(|decision| matches!(
        &decision.classification,
        StatementClassification::Conditional(conditions)
            if conditions.iter().any(contains_cookie_condition)
    )));
    assert!(report.statements.iter().any(|decision| matches!(
        &decision.classification,
        StatementClassification::HeaderMutation(mutation)
            if mutation.cookie && mutation.operation == HeaderOperation::Set
    )));
    assert!(report.statements.iter().any(|decision| matches!(
        &decision.classification,
        StatementClassification::Unsupported(UnsupportedBehavior::VmodCall { function, .. })
            if function == b"std.log"
    )));
    assert!(report.statements.iter().any(|decision| matches!(
        &decision.classification,
        StatementClassification::Unsupported(UnsupportedBehavior::FunctionCall { function })
            if function == b"custom_audit"
    )));
}

#[test]
fn included_declarations_retain_the_include_chain() {
    let report = representative_report();
    let backend = &report.backends[0];

    assert_eq!(backend.provenance.span.source(), SourceId::new(2));
    assert_eq!(backend.provenance.include_stack.len(), 1);
    assert_eq!(
        backend.provenance.include_stack[0].source(),
        SourceId::new(1)
    );
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| { diagnostic.code() != varnish::E_VCL_INCLUDE_NOT_FOUND })
    );
}

#[test]
fn include_cycles_are_blocking_and_do_not_expand_forever() {
    let root = source(1, "root.vcl", b"include \"child.vcl\";");
    let child = source(2, "child.vcl", b"include \"root.vcl\";");
    let report = analyze(&root, &[child]);

    assert!(report.has_errors());
    assert_eq!(report.sources.len(), 2);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code() == varnish::E_VCL_INCLUDE_CYCLE })
    );
}

#[test]
fn filesystem_v41_glob_report_covers_composition_and_native_facts() {
    let root = fixture("v41/root.vcl");
    let invocation = VarnishdInvocation::new([
        "varnishd",
        "-a",
        ":6081",
        "-s",
        "primary=malloc,256M",
        "-snone",
        "-p",
        "thread_pools=4",
        "-F",
    ]);
    let report = import(&root, &invocation);

    assert!(report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(
        report.lowering,
        LoweringStatus::Blocked(LoweringBlocker::UnsupportedBehavior)
    );
    assert!(report.source_graph.snapshot_stable);
    assert_eq!(report.source_graph.sources.len(), 3);
    assert_eq!(
        report.source_graph.includes[0]
            .targets
            .iter()
            .map(|target| {
                target
                    .requested_path
                    .file_name()
                    .expect("glob filename")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>(),
        ["10-origin.vcl", "20-policy.vcl"]
    );
    assert_eq!(
        decision_signatures(&report).join("\n"),
        EXPECTED_V41_DECISIONS.trim()
    );
    assert!(
        report
            .declarations
            .iter()
            .all(|declaration| { declaration.version.effective == Some(VclVersion::V4_1) })
    );
    assert!(report.declarations.iter().any(|declaration| {
        matches!(
            &declaration.classification,
            varnish::DeclarationClassification::Backend { name, .. } if name == b"origin"
        ) && declaration.version.origin == VersionOrigin::IncludeInherited
    }));
    assert_eq!(report.acls.len(), 1);
    assert_eq!(report.probes.len(), 1);
    assert_eq!(report.imports[0].alias, b"dir");
    assert_eq!(
        report.imports[0].from.as_deref(),
        Some(b"/usr/lib/varnish/vmods".as_slice())
    );
    assert!(matches!(report.backends[0].kind, BackendKind::None));
    assert!(matches!(report.backends[1].kind, BackendKind::Unix { .. }));
    assert!(
        report.backends[2]
            .properties
            .iter()
            .any(|property| matches!(
                property,
                BackendProperty::Probe(ProbeReference::Named { declaration: 0, .. })
            ))
    );
    assert_eq!(report.modern_directors.len(), 1);
    assert_eq!(report.modern_directors[0].methods.len(), 2);
    let recv = report
        .compositions
        .iter()
        .find(|composition| composition.name == b"vcl_recv")
        .expect("composed vcl_recv");
    assert_eq!(recv.fragments.len(), 3);
    assert_eq!(
        recv.built_in,
        BuiltinComposition::AppendedAfterUserFragments
    );
    assert!(
        report
            .call_graph
            .edges
            .iter()
            .any(|edge| { edge.caller == b"classify_request" && edge.callee == b"audit_request" })
    );
    assert!(report.call_graph.cycles.is_empty());
    assert!(report.statements.iter().any(|statement| matches!(
        statement.classification,
        StatementClassification::Feature(FeatureBehavior::Esi { .. })
    )));
    assert!(report.statements.iter().any(|statement| matches!(
        statement.classification,
        StatementClassification::Unsupported(UnsupportedBehavior::InlineC)
    )));
    assert_eq!(report.invocation.storage.len(), 2);
    assert_eq!(report.invocation.storage[0].kind, StorageKind::Malloc);
    assert_eq!(report.invocation.storage[1].kind, StorageKind::None);
}

#[test]
fn filesystem_v40_exact_include_inherits_version_and_legacy_director() {
    let report = import(&fixture("v40/root.vcl"), &VarnishdInvocation::default());

    assert!(report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(
        report.lowering,
        LoweringStatus::Blocked(LoweringBlocker::SemanticMismatch)
    );
    assert!(report.source_graph.snapshot_stable);
    assert_eq!(report.backends.len(), 2);
    assert_eq!(report.directors.len(), 1);
    assert_eq!(report.directors[0].members.len(), 2);
    assert!(
        report
            .declarations
            .iter()
            .all(|declaration| { declaration.version.effective == Some(VclVersion::V4_0) })
    );
}

#[test]
fn bounded_call_graph_records_custom_subroutine_cycles_without_execution() {
    let source = source(
        1,
        "calls.vcl",
        b"vcl 4.1; sub first { call second; } sub second { call first; }",
    );
    let report = analyze(&source, &[]);

    assert_eq!(report.call_graph.edges.len(), 2);
    assert!(!report.call_graph.cycles.is_empty());
    assert!(!report.call_graph.truncated);
    assert!(!report.call_graph.depth_limited);
}

#[test]
fn filesystem_glob_match_limit_is_terminal_and_bounded() {
    let report = load_with_limits(
        &fixture("v41/root.vcl"),
        VarnishLoadLimits {
            glob_matches: 1,
            ..VarnishLoadLimits::default()
        },
    );

    assert!(report.has_errors());
    assert!(report.value().includes[0].truncated);
    assert!(report.value().includes[0].targets.is_empty());
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| { diagnostic.code() == E_SOURCE_LIMIT })
    );
}

#[test]
fn invocation_model_retains_storage_and_startup_without_spawning_varnishd() {
    let facts = VarnishdInvocation::new([
        "varnishd",
        "-sfile,/var/lib/varnish/cache.bin,1G",
        "-a127.0.0.1:6081",
        "-p",
        "default_ttl=120",
        "--future-option",
    ])
    .facts();

    assert_eq!(facts.storage[0].kind, StorageKind::File);
    assert_eq!(facts.startup.len(), 2);
    assert_eq!(facts.unsupported_arguments, ["--future-option"]);

    let bounded = VarnishdInvocation::new(std::iter::repeat_n(
        "--unknown",
        MAX_INVOCATION_ARGUMENTS + 1,
    ))
    .facts();
    assert!(bounded.truncated);
    assert_eq!(
        bounded.unsupported_arguments.len(),
        MAX_INVOCATION_ARGUMENTS
    );
}

#[test]
fn exact_static_cache_subset_lowers_to_a_finalized_candidate() {
    let invocation = VarnishdInvocation::new([
        "varnishd",
        "-a",
        ":6081",
        "-s",
        "cache=malloc,256M",
        "-p",
        "default_ttl=120s",
        "-p",
        "default_grace=10s",
        "-p",
        "default_keep=300s",
        "-F",
    ]);
    let report = import(&fixture("exact.vcl"), &invocation);

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(report.lowering, LoweringStatus::Lowered);
    let config = report
        .candidate
        .config
        .as_ref()
        .expect("finalized candidate");
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.upstream_pools.len(), 1);
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(config.cache_stores.len(), 1);
    let oxiroute_config::HttpRouteAction::Proxy { policy, .. } =
        &config.http_services[0].routes[0].action
    else {
        panic!("exact Varnish route must proxy");
    };
    let cache = policy.cache.as_ref().expect("cache policy");
    assert_eq!(cache.store, "cache");
    assert_eq!(cache.default_ttl_ms, 120_000);
    assert_eq!(cache.grace_ms, 10_000);
    assert_eq!(cache.keep_ms, 300_000);
    assert!(policy.response_headers.iter().any(|header| matches!(
        header,
        oxiroute_config::HttpResponseHeaderMutation::Set { name, value, .. }
            if name == "x-cache" && value == "hit"
    )));
    assert!(
        report
            .candidate
            .provenance
            .iter()
            .any(|entry| entry.path == "/http_services/0/routes/0/action/policy/cache")
    );
}

#[test]
fn exact_http_probes_lower_named_and_default_health_checks() {
    let report = import(
        &fixture("health-probe.vcl"),
        &VarnishdInvocation::new([
            "varnishd",
            "-s",
            "cache=malloc,256M",
            "-p",
            "default_keep=300s",
        ]),
    );

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .candidate
        .config
        .as_ref()
        .expect("health probe candidate");
    let shared = config
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "origin")
        .expect("named probe pool");
    let shared_check = shared.health_check.as_ref().expect("named health check");
    assert_eq!(shared_check.kind, oxiroute_config::HealthCheckType::Http);
    assert_eq!(shared_check.interval_ms, 5_000);
    assert_eq!(shared_check.timeout_ms, 1_000);
    assert_eq!(shared_check.healthy_threshold, 1);
    assert_eq!(shared_check.unhealthy_threshold, 1);
    assert_eq!(
        shared_check.startup,
        oxiroute_config::HealthStartup::Healthy
    );
    assert_eq!(shared_check.path.as_deref(), Some("/ready"));
    assert_eq!(shared_check.expected_status, Some(204));

    let default = config
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "fallback")
        .expect("default probe pool");
    let default_check = default.health_check.as_ref().expect("default health check");
    assert_eq!(default_check.interval_ms, 10_000);
    assert_eq!(default_check.timeout_ms, 2_000);
    assert_eq!(
        default_check.startup,
        oxiroute_config::HealthStartup::Unhealthy
    );
    assert_eq!(default_check.path.as_deref(), Some("/"));
    assert_eq!(default_check.expected_status, None);

    let inline = config
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "inline")
        .expect("inline probe pool");
    let inline_check = inline.health_check.as_ref().expect("inline health check");
    assert_eq!(inline_check.path.as_deref(), Some("/inline-ready"));
    assert_eq!(inline_check.expected_status, None);

    let interval_provenance = report
        .candidate
        .provenance
        .iter()
        .find(|entry| entry.path == "/upstream_pools/0/health_check/interval_ms")
        .expect("health interval provenance");
    assert_eq!(
        interval_provenance.origins[0].span.source(),
        SourceId::new(0)
    );
}

#[test]
fn noncanonical_probe_window_blocks_lowering() {
    let source = source(
        1,
        "window.vcl",
        br#"vcl 4.1;
probe default {
    .timeout = 1s;
    .interval = 5s;
    .window = 2;
    .threshold = 1;
}
backend origin { .host = "127.0.0.1"; .port = 8080; }
sub vcl_recv { set req.backend_hint = origin; return (hash); }
sub vcl_hash { hash_data(req.url); return (lookup); }
sub vcl_backend_response { set beresp.ttl = 1s; return (deliver); }"#,
    );
    let report = analyze(&source, &[]);

    assert_eq!(
        report.lowering,
        LoweringStatus::Blocked(LoweringBlocker::SemanticMismatch)
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code() == varnish::E_VCL_SEMANTIC_MISMATCH })
    );
}

#[test]
fn custom_probe_request_blocks_lowering() {
    let source = source(
        1,
        "request.vcl",
        br#"vcl 4.1;
probe default {
    .request = "GET /ready HTTP/1.1";
    .timeout = 1s;
    .interval = 5s;
    .window = 1;
    .threshold = 1;
}
backend origin { .host = "127.0.0.1"; .port = 8080; }
sub vcl_recv { set req.backend_hint = origin; return (hash); }
sub vcl_hash { hash_data(req.url); return (lookup); }
sub vcl_backend_response { set beresp.ttl = 1s; return (deliver); }"#,
    );
    let report = analyze(&source, &[]);

    assert_eq!(
        report.lowering,
        LoweringStatus::Blocked(LoweringBlocker::SemanticMismatch)
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code() == varnish::E_VCL_SEMANTIC_MISMATCH })
    );
}

#[test]
fn non_set_probe_assignment_is_not_treated_as_a_supported_property() {
    let source = source(
        1,
        "non-set-probe.vcl",
        b"vcl 4.1; probe default { .window += 1; }",
    );
    let report = analyze(&source, &[]);

    assert!(matches!(
        report.probes[0].properties.first(),
        Some(varnish::ProbeProperty::Unsupported { .. })
    ));
}

#[test]
fn director_with_inconsistent_probe_policies_blocks_lowering() {
    let source = source(
        1,
        "director-probes.vcl",
        br#"vcl 4.1;
probe fast {
    .timeout = 1s;
    .interval = 5s;
    .window = 1;
    .threshold = 1;
}
probe slow {
    .timeout = 1s;
    .interval = 10s;
    .window = 1;
    .threshold = 1;
}
backend primary { .host = "127.0.0.1"; .port = 8080; .probe = fast; }
backend secondary { .host = "127.0.0.2"; .port = 8080; .probe = slow; }
director pool round-robin {
    { .backend = primary; }
    { .backend = secondary; }
}
sub vcl_recv { set req.backend_hint = pool; return (hash); }
sub vcl_hash { hash_data(req.url); return (lookup); }
sub vcl_backend_response { set beresp.ttl = 1s; return (deliver); }"#,
    );
    let report = analyze(&source, &[]);

    assert_eq!(
        report.lowering,
        LoweringStatus::Blocked(LoweringBlocker::SemanticMismatch)
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code() == varnish::E_VCL_SEMANTIC_MISMATCH })
    );
}

#[test]
fn exact_legacy_round_robin_director_lowers_to_one_upstream_pool() {
    let invocation = VarnishdInvocation::new([
        "varnishd",
        "-s",
        "cache=malloc,256M",
        "-p",
        "default_ttl=120s",
        "-p",
        "default_grace=10s",
        "-p",
        "default_keep=300s",
    ]);
    let report = import(&fixture("exact-director.vcl"), &invocation);

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .candidate
        .config
        .as_ref()
        .expect("finalized director candidate");
    let pool = config
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "pool")
        .expect("director pool");
    assert_eq!(pool.servers.len(), 2);
    assert_eq!(
        pool.algorithm,
        oxiroute_config::UpstreamAlgorithm::RoundRobin
    );
}

#[test]
fn exact_file_storage_lowers_to_a_disk_cache_store() {
    let invocation = VarnishdInvocation::new([
        "varnishd",
        "-s",
        "cache=file,/var/lib/varnish/cache.bin,1G",
        "-p",
        "default_ttl=120s",
        "-p",
        "default_grace=10s",
        "-p",
        "default_keep=300s",
    ]);
    let report = import(&fixture("exact.vcl"), &invocation);

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert!(matches!(
        report
            .candidate
            .config
            .as_ref()
            .expect("disk candidate")
            .cache_stores[0],
        oxiroute_config::CacheStore::Disk {
            ref root_directory,
            max_bytes,
            ..
        } if root_directory == std::path::Path::new("/var/lib/varnish/cache.bin")
            && max_bytes == 1 << 30
    ));
}

#[test]
fn vmod_calls_in_conditions_and_unknown_methods_are_distinct() {
    let source = source(
        1,
        "dynamic.vcl",
        b"vcl 4.1; import std as util; sub vcl_init { new helper = util.duration(\"1s\", 1s); helper.touch(); } sub vcl_recv { if (util.integer(req.http.X, 0) > 0) { return(pass); } mystery.object(); }",
    );
    let report = analyze(&source, &[]);

    let condition = report
        .statements
        .iter()
        .find_map(|statement| match &statement.classification {
            StatementClassification::Conditional(conditions) => conditions.first(),
            _ => None,
        })
        .expect("condition decision");
    assert!(matches!(
        condition,
        Condition::UnsupportedCall {
            behavior: UnsupportedBehavior::VmodCall { module, function },
            ..
        } if module == b"std" && function == b"util.integer"
    ));
    assert!(report.statements.iter().any(|statement| matches!(
        statement.classification,
        StatementClassification::Dynamic(_)
    )));
    assert_eq!(report.vmod_objects.len(), 1);
    assert!(report.statements.iter().any(|statement| matches!(
        &statement.classification,
        StatementClassification::Unsupported(UnsupportedBehavior::VmodCall { module, function })
            if module == b"std" && function == b"helper.touch"
    )));
}

#[test]
fn duplicate_backend_identities_are_reported_before_lowering() {
    let source = source(
        1,
        "duplicates.vcl",
        b"vcl 4.1; backend repeated none; backend repeated none;",
    );
    let report = analyze(&source, &[]);

    assert!(report.has_errors());
    assert_eq!(
        report
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == E_DUPLICATE_IDENTITY)
            .count(),
        2
    );
}

#[test]
fn inline_backend_probe_is_typed_without_evaluation() {
    let source = source(
        1,
        "inline-probe.vcl",
        b"vcl 4.1; backend origin { .host = \"192.0.2.20\"; .probe = { .url = \"/ready\"; .interval = 2s; .threshold = 3; } }",
    );
    let report = analyze(&source, &[]);

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert!(
        report.backends[0]
            .properties
            .iter()
            .any(|property| matches!(
                property,
                BackendProperty::Probe(ProbeReference::Inline(properties)) if properties.len() == 3
            ))
    );
}

fn representative_report() -> ImportReport {
    let root = source(1, "representative.vcl", REPRESENTATIVE);
    let included = source(2, "shared-backends.vcl", SHARED_BACKENDS);
    analyze(&root, &[included])
}

fn representative_statement_count() -> usize {
    let root = source(1, "representative.vcl", REPRESENTATIVE);
    parse(&root)
        .value()
        .declarations
        .iter()
        .map(|declaration| match declaration {
            Declaration::Subroutine(subroutine) => count_statements(&subroutine.statements),
            _ => 0,
        })
        .sum()
}

fn count_statements(statements: &[varnish::Statement]) -> usize {
    statements
        .iter()
        .map(|statement| {
            1 + match &statement.kind {
                StatementKind::If(if_statement) => {
                    if_statement
                        .branches
                        .iter()
                        .map(|branch| count_statements(&branch.statements))
                        .sum::<usize>()
                        + count_statements(&if_statement.otherwise)
                }
                _ => 0,
            }
        })
        .sum()
}

fn contains_cookie_condition(condition: &Condition) -> bool {
    match condition {
        Condition::Cookie { .. } => true,
        Condition::All(left, right) | Condition::Any(left, right) => {
            contains_cookie_condition(left) || contains_cookie_condition(right)
        }
        Condition::Not(condition) => contains_cookie_condition(condition),
        Condition::Header { .. }
        | Condition::Acl { .. }
        | Condition::Comparison { .. }
        | Condition::Value(_)
        | Condition::Dynamic(_)
        | Condition::UnsupportedCall { .. } => false,
    }
}

fn source(id: u32, name: &str, bytes: &'static [u8]) -> SourceFile {
    SourceFile::new(SourceId::new(id), name, bytes)
}

fn fixture(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures/varnish")
        .join(relative)
}
