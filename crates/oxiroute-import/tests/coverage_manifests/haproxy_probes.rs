use std::{collections::BTreeSet, fmt::Write as _};

use oxiroute_config::{
    HttpHostSelector, HttpPathSelector, ListenerBind, UpstreamAlgorithm, UpstreamEndpoint,
};
use oxiroute_import::{
    DiagnosticStage, Report,
    haproxy::{
        BlockingReason, CanonicalCandidate as HaproxyCandidate, Configuration, DecisionOutcome,
        E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, EffectiveConfiguration, SectionKind,
        import_parsed as import_haproxy, resolve_parsed,
    },
};
use syn::Item;

use crate::{
    manifests::{DirectiveForm, DirectiveManifest, Disposition},
    report_invariants::{
        HaproxyProbeReports, assert_haproxy_report_invariants, parse_haproxy_source,
    },
    support::{assert_set_equality, read_manifest, read_source},
};

#[test]
fn haproxy_exposes_only_diagnostic_carrying_resolution_and_import_entry_points() {
    let module = read_source("crates/oxiroute-import/src/haproxy/mod.rs");
    let resolver = read_source("crates/oxiroute-import/src/haproxy/resolver.rs");
    let lower = read_source("crates/oxiroute-import/src/haproxy/lower/mod.rs");

    for entry_point in [
        "pub fn resolve_parsed",
        "pub fn analyze_sources",
        "pub fn analyze_roots",
        "pub fn import_parsed",
        "pub fn import_sources",
        "pub fn import_roots",
    ] {
        assert!(module.contains(entry_point), "missing safe `{entry_point}`");
    }
    assert!(!resolver.contains("pub fn resolve("));
    assert!(!resolver.contains("pub fn resolve_report("));
    assert!(!lower.contains("pub fn lower("));
}

#[test]
fn haproxy_manifest_forms_execute_parser_semantic_and_lowering_decisions() {
    let manifest: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    assert_haproxy_lowered_subset(&manifest.entries);
    let registered_sections = haproxy_parser_registered_sections();
    let manifested_sections = manifest
        .entries
        .iter()
        .filter(|entry| entry.id.starts_with("directive.haproxy.section."))
        .map(|entry| entry.key.clone())
        .collect::<BTreeSet<_>>();
    assert_set_equality(
        "HAProxy parser section registrations",
        &registered_sections,
        &manifested_sections,
    );
    for entry in &manifest.entries {
        let (parsed, resolved, lowered) = execute_haproxy_probe(entry);
        assert_haproxy_entry_probe(entry, &parsed, &resolved, &lowered, "primary form", false);
    }
}

#[test]
fn haproxy_manifest_contexts_and_http_matching_requirements_are_executable() {
    let manifest: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    for entry in &manifest.entries {
        for context in &entry.contexts {
            for directive in haproxy_context_directives(entry, context) {
                let source = haproxy_context_probe_source(entry, context, directive);
                let parsed = parse_haproxy_source("coverage-context.cfg", source.as_bytes());
                assert_haproxy_parsed_context(parsed.value(), entry, context);
                let resolved = resolve_parsed(parsed.clone());
                let lowered = import_haproxy(parsed.clone());
                assert_haproxy_entry_probe(entry, &parsed, &resolved, &lowered, context, true);
            }
        }
    }

    let raw_path = import_haproxy(parse_haproxy_source(
        "raw-path.cfg",
        haproxy_http_base().as_bytes(),
    ));
    let raw_config = raw_path
        .value()
        .config
        .as_ref()
        .expect("raw path finalizes");
    assert!(matches!(
        raw_config.http_services[0].routes[0].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/api"
    ));

    let host = haproxy_http_base().replace(
        "acl api path_beg /api",
        "acl api hdr(host) api.example.test",
    );
    let host_header = import_haproxy(parse_haproxy_source("host.cfg", host.as_bytes()));
    let host_config = host_header
        .value()
        .config
        .as_ref()
        .expect("exact authority finalizes");
    assert!(matches!(
        host_config.http_services[0].routes[0].host,
        Some(HttpHostSelector::ExactAuthority { ref value }) if value == "api.example.test"
    ));

    let static_tcp = import_haproxy(parse_haproxy_source(
        "static-tcp.cfg",
        haproxy_tcp_base().as_bytes(),
    ));
    assert!(static_tcp.value().config.is_some());

    let unix_listener_source =
        haproxy_tcp_base().replace("bind 127.0.0.1:15432", "bind /run/coverage.sock");
    let unix_listener = import_haproxy(parse_haproxy_source(
        "unix-listener.cfg",
        unix_listener_source.as_bytes(),
    ));
    assert!(matches!(
        unix_listener.value().config.as_ref().unwrap().listeners[0].bind,
        ListenerBind::Unix { ref path, .. } if path == std::path::Path::new("/run/coverage.sock")
    ));

    let dns_source = haproxy_tcp_base().replace(
        "server primary 127.0.0.1:5432",
        "server primary database.internal:5432",
    );
    let dns = import_haproxy(parse_haproxy_source("dns.cfg", dns_source.as_bytes()));
    assert!(matches!(
        dns.value().config.as_ref().unwrap().upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Dns { ref host, port: 5432 } if host == "database.internal"
    ));

    let unix_source = haproxy_tcp_base().replace(
        "server primary 127.0.0.1:5432",
        "server primary /run/database.sock",
    );
    let unix = import_haproxy(parse_haproxy_source("unix.cfg", unix_source.as_bytes()));
    assert!(matches!(
        unix.value().config.as_ref().unwrap().upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Unix { ref path } if path == std::path::Path::new("/run/database.sock")
    ));

    let least_source = haproxy_tcp_base().replace("balance roundrobin", "balance leastconn");
    let least = import_haproxy(parse_haproxy_source("least.cfg", least_source.as_bytes()));
    assert_eq!(
        least.value().config.as_ref().unwrap().upstream_pools[0].algorithm,
        UpstreamAlgorithm::LeastConnections
    );

    let unbounded_http = import_haproxy(parse_haproxy_source(
        "unbounded-http.cfg",
        haproxy_strict_http_base().as_bytes(),
    ));
    let http_config = unbounded_http
        .value()
        .config
        .as_ref()
        .expect("strict HTTP probe finalizes");
    assert_eq!(http_config.http_services[0].max_request_body_bytes, None);
}

fn assert_haproxy_lowered_subset(entries: &[DirectiveForm]) {
    let lowered = entries
        .iter()
        .filter(|entry| entry.disposition == Disposition::Lowered)
        .map(|entry| entry.id.as_str())
        .collect::<BTreeSet<_>>();
    let expected = [
        "directive.haproxy.balance.roundrobin-tcp",
        "directive.haproxy.maxconn.global",
        "directive.haproxy.bind.tcp",
        "directive.haproxy.bind.unix-tcp",
        "directive.haproxy.default-backend.tcp",
        "directive.haproxy.maxconn.proxy-tcp",
        "directive.haproxy.mode.tcp",
        "directive.haproxy.no-option.forwardfor-tcp",
        "directive.haproxy.no-option.httpchk-tcp",
        "directive.haproxy.no-option.redispatch-tcp",
        "directive.haproxy.retries.zero-tcp",
        "directive.haproxy.balance.leastconn-tcp",
        "directive.haproxy.balance.first-tcp",
        "directive.haproxy.server.dns-tcp",
        "directive.haproxy.server.static-ip-tcp",
        "directive.haproxy.server.unix-tcp",
        "directive.haproxy.server.health-options",
        "directive.haproxy.default-server.health-options",
        "directive.haproxy.maxconn.proxy-http",
        "directive.haproxy.mode.http",
        "directive.haproxy.bind.http",
        "directive.haproxy.bind.unix-http",
        "directive.haproxy.default-backend.http",
        "directive.haproxy.balance.roundrobin-http",
        "directive.haproxy.balance.leastconn-http",
        "directive.haproxy.balance.first-http",
        "directive.haproxy.server.static-ip-http",
        "directive.haproxy.server.dns-http",
        "directive.haproxy.server.unix-http",
        "directive.haproxy.retries.zero-http",
        "directive.haproxy.acl.path-beg",
        "directive.haproxy.acl.hdr-host",
        "directive.haproxy.use-backend",
        "directive.haproxy.option.redispatch-bare-http",
        "directive.haproxy.no-option.redispatch-http",
        "directive.haproxy.no-option.forwardfor-http",
        "directive.haproxy.option.forwardfor",
        "directive.haproxy.no-option.httpchk-http",
        "directive.haproxy.option.http-server-close",
        "directive.haproxy.http-check.send",
        "directive.haproxy.http-request.return",
        "directive.haproxy.http-request.redirect",
        "directive.haproxy.http-request.set-header",
        "directive.haproxy.http-request.del-header",
        "directive.haproxy.http-response.set-header",
        "directive.haproxy.http-response.del-header",
        "directive.haproxy.stats-page",
        "directive.haproxy.use-backend.host-path-conjunction",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        lowered, expected,
        "HAProxy lowered subset changed without executable coverage"
    );
}

fn haproxy_parser_registered_sections() -> BTreeSet<String> {
    const KINDS: [SectionKind; 21] = [
        SectionKind::Global,
        SectionKind::Defaults,
        SectionKind::Frontend,
        SectionKind::Backend,
        SectionKind::Listen,
        SectionKind::Userlist,
        SectionKind::Peers,
        SectionKind::Mailers,
        SectionKind::NamespaceList,
        SectionKind::Traces,
        SectionKind::Resolvers,
        SectionKind::Cache,
        SectionKind::FcgiApp,
        SectionKind::Ring,
        SectionKind::LogForward,
        SectionKind::LogProfile,
        SectionKind::HttpErrors,
        SectionKind::CrtStore,
        SectionKind::Acme,
        SectionKind::Healthcheck,
        SectionKind::Program,
    ];
    let parser_source = read_source("crates/oxiroute-import/src/haproxy/parser.rs");
    let parser = syn::parse_file(&parser_source).expect("parse HAProxy parser registry");
    let variant_count = parser
        .items
        .iter()
        .find_map(|item| match item {
            Item::Enum(item) if item.ident == "SectionKind" => Some(item.variants.len()),
            _ => None,
        })
        .expect("find SectionKind parser registry");
    assert_eq!(
        KINDS.len(),
        variant_count,
        "SectionKind variants changed without an executable coverage mapping"
    );

    KINDS
        .into_iter()
        .map(|kind| {
            let keyword = haproxy_section_keyword(kind);
            let source = if kind == SectionKind::Global {
                format!("{keyword}\n")
            } else {
                format!("{keyword} coverage\n")
            };
            let parsed = parse_haproxy_source("section-probe.cfg", source.as_bytes());
            assert!(parsed.diagnostics().is_empty(), "{keyword}");
            assert_eq!(
                parsed.value().files[0].document.sections.len(),
                1,
                "{keyword}"
            );
            assert_eq!(
                parsed.value().files[0].document.sections[0].kind,
                kind,
                "{keyword} was not parser-registered"
            );
            let resolved = resolve_parsed(parsed.clone());
            assert_haproxy_report_invariants(parsed.value(), resolved.value());
            assert!(resolved.value().ledger.iter().all(|decision| {
                !matches!(
                    decision.outcome,
                    DecisionOutcome::Blocked(
                        BlockingReason::UnknownDirective | BlockingReason::UnconsumedDirective
                    )
                )
            }));
            keyword.to_owned()
        })
        .collect()
}

const fn haproxy_section_keyword(kind: SectionKind) -> &'static str {
    match kind {
        SectionKind::Global => "global",
        SectionKind::Defaults => "defaults",
        SectionKind::Frontend => "frontend",
        SectionKind::Backend => "backend",
        SectionKind::Listen => "listen",
        SectionKind::Userlist => "userlist",
        SectionKind::Peers => "peers",
        SectionKind::Mailers => "mailers",
        SectionKind::NamespaceList => "namespace_list",
        SectionKind::Traces => "traces",
        SectionKind::Resolvers => "resolvers",
        SectionKind::Cache => "cache",
        SectionKind::FcgiApp => "fcgi-app",
        SectionKind::Ring => "ring",
        SectionKind::LogForward => "log-forward",
        SectionKind::LogProfile => "log-profile",
        SectionKind::HttpErrors => "http-errors",
        SectionKind::CrtStore => "crt-store",
        SectionKind::Acme => "acme",
        SectionKind::Healthcheck => "healthcheck",
        SectionKind::Program => "program",
    }
}

fn execute_haproxy_probe(entry: &DirectiveForm) -> HaproxyProbeReports {
    let source = haproxy_probe_source(entry);
    let parsed = parse_haproxy_source("coverage-probe.cfg", source.as_bytes());
    let resolved = resolve_parsed(parsed.clone());
    let lowered = import_haproxy(parsed.clone());
    (parsed, resolved, lowered)
}

fn assert_haproxy_entry_probe(
    entry: &DirectiveForm,
    parsed: &Report<Configuration>,
    resolved: &Report<EffectiveConfiguration>,
    lowered: &Report<HaproxyCandidate>,
    label: &str,
    allow_superseded_blocker: bool,
) {
    assert_haproxy_report_invariants(parsed.value(), resolved.value());
    assert_eq!(
        resolved
            .diagnostics()
            .iter()
            .filter(|diagnostic| matches!(
                diagnostic.code(),
                E_UNKNOWN_DIRECTIVE | E_UNCONSUMED_DIRECTIVE
            ))
            .count(),
        0,
        "{} reached a generic unknown/unconsumed fallback ({label})",
        entry.id
    );
    let decisions = resolved
        .value()
        .ledger
        .iter()
        .filter(|decision| decision.keyword == entry.key.as_bytes())
        .collect::<Vec<_>>();
    assert!(
        !decisions.is_empty(),
        "{} was not parser-registered ({label})",
        entry.id
    );
    match entry.disposition {
        Disposition::Lowered => {
            assert!(
                decisions.iter().any(|decision| matches!(
                    decision.outcome,
                    DecisionOutcome::Consumed(_) | DecisionOutcome::Superseded { .. }
                )),
                "{} was not semantically consumed ({label})",
                entry.id
            );
            assert!(
                lowered.value().config.is_some(),
                "{} claims lowering but its {label} probe did not finalize: {:?}",
                entry.id,
                lowered.diagnostics()
            );
        }
        Disposition::Classified => assert!(
            decisions.iter().any(|decision| matches!(
                decision.outcome,
                DecisionOutcome::Consumed(_) | DecisionOutcome::Superseded { .. }
            )),
            "{} was not semantically classified ({label})",
            entry.id
        ),
        Disposition::Blocked => {
            let superseded = decisions
                .iter()
                .any(|decision| matches!(decision.outcome, DecisionOutcome::Superseded { .. }));
            if !superseded || !allow_superseded_blocker {
                assert!(
                    lowered.value().config.is_none(),
                    "{} silently finalized ({label})",
                    entry.id
                );
                assert!(
                    decisions
                        .iter()
                        .any(|decision| matches!(decision.outcome, DecisionOutcome::Blocked(_)))
                        || lowered
                            .diagnostics()
                            .iter()
                            .any(|diagnostic| diagnostic.stage() == DiagnosticStage::Lower),
                    "{} has no semantic or lowering blocker ({label})",
                    entry.id
                );
            }
        }
        Disposition::Externalized => {
            assert!(
                decisions.iter().any(|decision| {
                    matches!(decision.outcome, DecisionOutcome::Externalized(_))
                })
            );
            assert!(
                lowered.value().config.is_some(),
                "{} blocks activation ({label})",
                entry.id
            );
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the exhaustive directive fixture registry stays adjacent to its manifest IDs"
)]
fn haproxy_context_directives(entry: &DirectiveForm, context: &str) -> Vec<&'static str> {
    if entry.id.starts_with("directive.haproxy.section.") {
        return vec![""];
    }
    if let Some(directives) = haproxy_process_context_directives(&entry.id, context) {
        return directives;
    }
    match entry.id.as_str() {
        "directive.haproxy.maxconn.proxy-tcp"
        | "directive.haproxy.maxconn.proxy-http"
        | "directive.haproxy.maxconn.global" => vec!["maxconn 1000"],
        "directive.haproxy.mode.tcp" => vec!["mode tcp"],
        "directive.haproxy.mode.http" => vec!["mode http"],
        "directive.haproxy.bind.tcp" | "directive.haproxy.bind.http" => {
            vec!["bind 127.0.0.1:15432"]
        }
        "directive.haproxy.bind.unix-tcp" | "directive.haproxy.bind.unix-http" => {
            vec!["bind /run/coverage.sock"]
        }
        "directive.haproxy.default-backend.tcp" => vec!["default_backend database_pool"],
        "directive.haproxy.default-backend.http" if context == "listen" => {
            vec!["default_backend database_pool"]
        }
        "directive.haproxy.default-backend.http" => vec!["default_backend fallback"],
        "directive.haproxy.balance.roundrobin-tcp"
        | "directive.haproxy.balance.roundrobin-http" => vec!["balance roundrobin"],
        "directive.haproxy.balance.leastconn-tcp" => vec!["balance leastconn"],
        "directive.haproxy.balance.leastconn-http" => {
            vec!["balance leastconn\noption http-server-close"]
        }
        "directive.haproxy.balance.first-tcp" => vec!["balance first"],
        "directive.haproxy.balance.first-http" => {
            vec!["balance first\noption http-server-close"]
        }
        "directive.haproxy.server.static-ip-tcp" | "directive.haproxy.server.static-ip-http" => {
            vec!["server primary 127.0.0.1:5432"]
        }
        "directive.haproxy.server.dns-tcp" | "directive.haproxy.server.dns-http" => {
            vec!["server primary database.lan:5432"]
        }
        "directive.haproxy.server.unix-tcp" | "directive.haproxy.server.unix-http" => {
            vec!["server primary /run/database.sock"]
        }
        "directive.haproxy.server.health-options" => {
            vec!["server primary 127.0.0.1:5432 check inter 2s rise 2 fall 3"]
        }
        "directive.haproxy.default-server.health-options" => {
            vec!["default-server check inter 2s fastinter 1s downinter 5s rise 2 fall 3"]
        }
        "directive.haproxy.retries.zero-tcp" | "directive.haproxy.retries.zero-http" => {
            vec!["retries 0"]
        }
        "directive.haproxy.retries.positive" => vec!["retries 3"],
        "directive.haproxy.timeout" => vec![
            "timeout client 30s",
            "timeout connect 30s",
            "timeout queue 30s",
            "timeout server 30s",
            "timeout http-request 30s",
            "timeout http-keep-alive 30s",
        ],
        "directive.haproxy.acl.path-beg" => vec!["acl api path_beg /api"],
        "directive.haproxy.acl.hdr-host" => {
            vec!["acl api hdr(host) api.example.test"]
        }
        "directive.haproxy.use-backend.host-path-conjunction" if context == "listen" => vec![
            "acl coverage_host hdr(host) -i api.example.test\nacl coverage_path path_beg /api\nuse_backend database_pool if coverage_host coverage_path",
        ],
        "directive.haproxy.use-backend.host-path-conjunction" => vec![
            "acl coverage_host hdr(host) -i api.example.test\nacl coverage_path path_beg /api\nuse_backend api if coverage_host coverage_path",
        ],
        "directive.haproxy.use-backend.dynamic-expression" if context == "listen" => {
            vec!["use_backend database_pool if { path /api }"]
        }
        "directive.haproxy.use-backend.dynamic-expression" => {
            vec!["use_backend api if { path /api }"]
        }
        "directive.haproxy.use-backend" if context == "listen" => {
            vec!["acl coverage path_beg /api\nuse_backend database_pool if coverage"]
        }
        "directive.haproxy.use-backend" => {
            vec!["acl coverage path_beg /api\nuse_backend api if coverage"]
        }
        "directive.haproxy.option.redispatch" | "directive.haproxy.option.redispatch-bare-http" => {
            vec!["option redispatch"]
        }
        "directive.haproxy.no-option.redispatch-tcp"
        | "directive.haproxy.no-option.redispatch-http" => vec!["no option redispatch"],
        "directive.haproxy.option.forwardfor" => vec!["option forwardfor"],
        "directive.haproxy.no-option.forwardfor-tcp"
        | "directive.haproxy.no-option.forwardfor-http" => vec!["no option forwardfor"],
        "directive.haproxy.option.httpchk" => vec!["option httpchk GET /health"],
        "directive.haproxy.option.http-server-close" => vec!["option http-server-close"],
        "directive.haproxy.no-option.httpchk-tcp" | "directive.haproxy.no-option.httpchk-http" => {
            vec!["no option httpchk"]
        }
        "directive.haproxy.http-check.expect-status" => {
            vec!["http-check expect status 200"]
        }
        "directive.haproxy.http-check.send" => {
            vec!["http-check send meth GET uri /health ver HTTP/1.1 hdr Host app.internal"]
        }
        "directive.haproxy.http-request.return" => {
            vec!["http-request return status 200 content-type text/plain string healthy"]
        }
        "directive.haproxy.http-request.redirect" => {
            vec!["http-request redirect location https://example.test/new code 308"]
        }
        "directive.haproxy.http-request.set-header" => {
            vec!["http-request set-header X-Client-IP %[src]"]
        }
        "directive.haproxy.http-request.del-header" => {
            vec!["http-request del-header X-Remove"]
        }
        "directive.haproxy.http-response.set-header" => {
            vec!["http-response set-header X-Frame-Options same-origin"]
        }
        "directive.haproxy.http-response.del-header" => {
            vec!["http-response del-header X-Powered-By"]
        }
        "directive.haproxy.stats-page" => {
            vec!["stats enable\nstats uri /stats\nstats refresh 10s\nstats admin if LOCALHOST"]
        }
        id => panic!("HAProxy manifest form has no context directive: {id}"),
    }
}

fn haproxy_process_context_directives(id: &str, context: &str) -> Option<Vec<&'static str>> {
    let directives = match id {
        "directive.haproxy.stats" => vec!["stats hide-version"],
        "directive.haproxy.log" if context == "global" => {
            vec!["log 127.0.0.1:514 local0"]
        }
        "directive.haproxy.log" => vec!["log global"],
        "directive.haproxy.log-format" => vec!["log-format coverage"],
        "directive.haproxy.error-log-format" => vec!["error-log-format coverage"],
        "directive.haproxy.unique-id-format" => vec!["unique-id-format coverage"],
        "directive.haproxy.unique-id-header" => vec!["unique-id-header X-Coverage"],
        "directive.haproxy.option.logging" => vec!["option httplog"],
        "directive.haproxy.chroot" => vec!["chroot /tmp"],
        "directive.haproxy.cpu-map" => vec!["cpu-map auto:1/1 0"],
        "directive.haproxy.daemon" => vec!["daemon"],
        "directive.haproxy.group" => vec!["group coverage"],
        "directive.haproxy.master-worker" => vec!["master-worker"],
        "directive.haproxy.nbproc" => vec!["nbproc 1"],
        "directive.haproxy.nbthread" => vec!["nbthread 1"],
        "directive.haproxy.pidfile" => vec!["pidfile /tmp/coverage.pid"],
        "directive.haproxy.setgid" => vec!["setgid 1000"],
        "directive.haproxy.setuid" => vec!["setuid 1000"],
        "directive.haproxy.user" => vec!["user coverage"],
        _ => return None,
    };
    Some(directives)
}

fn haproxy_context_probe_source(entry: &DirectiveForm, context: &str, directive: &str) -> String {
    if entry.id.starts_with("directive.haproxy.section.") {
        return haproxy_probe_source(entry);
    }
    if entry.id == "directive.haproxy.http-check.send" {
        return haproxy_http_check_send_probe_source(context, directive);
    }
    if entry.id == "directive.haproxy.stats-page" {
        return format!(
            "{context} coverage-stats\n  mode http\n  bind 127.0.0.1:18404\n  {directive}\n"
        );
    }
    let mut source = if context == "listen" {
        if haproxy_http_form(&entry.id) {
            haproxy_http_listen_base()
        } else {
            haproxy_tcp_listen_base()
        }
    } else if haproxy_http_form(&entry.id) {
        haproxy_http_base()
    } else {
        haproxy_tcp_base()
    };
    if context == "global" {
        source.insert_str(0, "global\n");
    }
    inject_haproxy_context(&source, context, &entry.key, directive)
}

fn haproxy_http_check_send_probe_source(context: &str, directive: &str) -> String {
    let mut source = if context == "listen" {
        haproxy_http_listen_base()
    } else {
        haproxy_http_base()
    };
    source = source
        .replace(
            "\nbackend api\n",
            "\nbackend api\n  http-check expect status 200\n",
        )
        .replace(
            "\nbackend fallback\n",
            "\nbackend fallback\n  http-check expect status 200\n",
        )
        .replace(
            "\nbackend database_pool\n",
            "\nbackend database_pool\n  http-check expect status 200\n",
        )
        .replace(
            "server api-1 127.0.0.1:3001",
            "server api-1 127.0.0.1:3001 check inter 1s",
        )
        .replace(
            "server fallback-1 127.0.0.1:3002",
            "server fallback-1 127.0.0.1:3002 check inter 1s",
        )
        .replace(
            "server local 127.0.0.1:3001",
            "server local 127.0.0.1:3001 check inter 1s",
        );
    if context == "listen" {
        source = source.replace(
            "listen public\n",
            "listen public\n  http-check expect status 200\n",
        );
    }
    inject_haproxy_context(&source, context, "__coverage_http_check_send__", directive)
}

fn inject_haproxy_context(source: &str, context: &str, key: &str, directive: &str) -> String {
    let section = if context == "named_defaults" {
        "defaults"
    } else {
        context
    };
    let mut rendered = String::with_capacity(source.len() + directive.len() + 8);
    let mut in_target = false;
    let mut found = false;
    let inherited_probe = section == "defaults";
    for line in source.lines() {
        if !line.starts_with(char::is_whitespace) {
            let keyword = line.split_ascii_whitespace().next().unwrap_or("");
            in_target = keyword == section;
            if in_target {
                found = true;
            }
            writeln!(&mut rendered, "{line}").expect("write HAProxy context fixture");
            if in_target {
                for directive_line in directive.lines() {
                    writeln!(&mut rendered, "  {directive_line}")
                        .expect("write HAProxy context directive");
                }
            }
            continue;
        }
        let keyword = line.split_ascii_whitespace().next().unwrap_or("");
        if keyword != key || (!in_target && !inherited_probe) {
            writeln!(&mut rendered, "{line}").expect("write HAProxy context fixture");
        }
    }
    assert!(found, "HAProxy context fixture has no `{context}` section");
    rendered
}

fn assert_haproxy_parsed_context(
    configuration: &Configuration,
    entry: &DirectiveForm,
    context: &str,
) {
    let document = &configuration.files[0].document;
    if entry.id.starts_with("directive.haproxy.section.") {
        assert!(
            document
                .sections
                .iter()
                .any(|section| section.header.name.value == entry.key.as_bytes())
        );
        return;
    }
    let kind = match context {
        "global" => SectionKind::Global,
        "defaults" | "named_defaults" => SectionKind::Defaults,
        "frontend" => SectionKind::Frontend,
        "backend" => SectionKind::Backend,
        "listen" => SectionKind::Listen,
        value => panic!("unknown HAProxy manifest context `{value}`"),
    };
    assert!(
        document.sections.iter().any(|section| {
            section.kind == kind
                && section
                    .directives
                    .iter()
                    .any(|directive| directive.name.value == entry.key.as_bytes())
        }),
        "{} has no parsed `{}` directive in {context}",
        entry.id,
        entry.key
    );
}

fn haproxy_http_form(id: &str) -> bool {
    id.ends_with("-http")
        || id
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix == "http")
        || id.starts_with("directive.haproxy.acl.")
        || id.starts_with("directive.haproxy.http-request.")
        || id.starts_with("directive.haproxy.http-response.")
        || matches!(
            id,
            "directive.haproxy.use-backend"
                | "directive.haproxy.use-backend.host-path-conjunction"
                | "directive.haproxy.use-backend.dynamic-expression"
                | "directive.haproxy.option.redispatch-bare-http"
                | "directive.haproxy.option.forwardfor"
                | "directive.haproxy.option.httpchk"
                | "directive.haproxy.option.http-server-close"
                | "directive.haproxy.http-check.expect-status"
                | "directive.haproxy.http-check.send"
        )
}

#[expect(
    clippy::too_many_lines,
    reason = "the exhaustive directive fixture registry stays adjacent to its manifest IDs"
)]
fn haproxy_probe_source(entry: &DirectiveForm) -> String {
    if entry.id.starts_with("directive.haproxy.section.") {
        return if entry.key == "global" {
            "global\n".into()
        } else {
            format!("{} coverage\n", entry.key)
        };
    }
    if let Some(source) = haproxy_transport_probe(&entry.id) {
        return source;
    }

    match entry.id.as_str() {
        "directive.haproxy.maxconn.global" => {
            format!("global\n  maxconn 4096\n{}", haproxy_tcp_base())
        }
        "directive.haproxy.server.health-options" => haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary 127.0.0.1:5432 check inter 2s rise 2 fall 3",
        ),
        "directive.haproxy.default-server.health-options" => inject_haproxy_defaults(
            "default-server check inter 2s fastinter 1s downinter 5s rise 2 fall 3",
        ),
        "directive.haproxy.retries.positive" => {
            haproxy_tcp_base().replace("retries 0", "retries 3")
        }
        "directive.haproxy.acl.path-beg" | "directive.haproxy.use-backend" => haproxy_http_base(),
        "directive.haproxy.acl.hdr-host" => haproxy_http_base().replace(
            "acl api path_beg /api",
            "acl api hdr(host) api.example.test",
        ),
        "directive.haproxy.use-backend.host-path-conjunction" => haproxy_http_base()
            .replace(
                "acl api path_beg /api",
                "acl api_host hdr(host) -i api.example.test\n  acl api_path path_beg /api",
            )
            .replace("use_backend api if api", "use_backend api if api_host api_path"),
        "directive.haproxy.use-backend.dynamic-expression" => {
            haproxy_http_base().replace("use_backend api if api", "use_backend api if { path /api }")
        }
        "directive.haproxy.http-request.return" => haproxy_terminal_return_base(),
        "directive.haproxy.http-request.redirect" => haproxy_terminal_redirect_base(),
        "directive.haproxy.http-request.set-header"
        | "directive.haproxy.http-request.del-header"
        | "directive.haproxy.http-response.set-header"
        | "directive.haproxy.http-response.del-header" => haproxy_header_mutation_base(),
        "directive.haproxy.option.redispatch" => inject_haproxy_defaults("option redispatch"),
        "directive.haproxy.option.redispatch-bare-http" => {
            inject_haproxy_http_defaults("option redispatch")
        }
        "directive.haproxy.no-option.redispatch-tcp" => {
            inject_haproxy_defaults("no option redispatch")
        }
        "directive.haproxy.no-option.redispatch-http" => {
            inject_haproxy_http_defaults("no option redispatch")
        }
        "directive.haproxy.option.forwardfor" => inject_haproxy_http_defaults("option forwardfor"),
        "directive.haproxy.no-option.forwardfor-tcp" => {
            inject_haproxy_defaults("no option forwardfor")
        }
        "directive.haproxy.no-option.forwardfor-http" => {
            inject_haproxy_http_defaults("no option forwardfor")
        }
        "directive.haproxy.option.httpchk" => {
            inject_haproxy_http_defaults("option httpchk GET /health")
        }
        "directive.haproxy.option.http-server-close" => {
            inject_haproxy_http_defaults("option http-server-close")
        }
        "directive.haproxy.no-option.httpchk-tcp" => inject_haproxy_defaults("no option httpchk"),
        "directive.haproxy.no-option.httpchk-http" => {
            inject_haproxy_http_defaults("no option httpchk")
        }
        "directive.haproxy.http-check.expect-status" => {
            inject_haproxy_http_defaults("http-check expect status 200")
        }
        "directive.haproxy.http-check.send" => haproxy_http_check_send_probe_source(
            "backend",
            "http-check send meth GET uri /health ver HTTP/1.1 hdr Host app.internal",
        ),
        "directive.haproxy.stats" => inject_haproxy_defaults("stats hide-version"),
        "directive.haproxy.stats-page" => "frontend coverage-stats\n  mode http\n  bind 127.0.0.1:18404\n  stats enable\n  stats uri /stats\n  stats refresh 10s\n  stats admin if LOCALHOST\n".into(),
        "directive.haproxy.log" => inject_haproxy_defaults("log global"),
        "directive.haproxy.log-format" => inject_haproxy_defaults("log-format coverage"),
        "directive.haproxy.error-log-format" => {
            inject_haproxy_defaults("error-log-format coverage")
        }
        "directive.haproxy.unique-id-format" => {
            inject_haproxy_defaults("unique-id-format coverage")
        }
        "directive.haproxy.unique-id-header" => {
            inject_haproxy_defaults("unique-id-header X-Coverage")
        }
        "directive.haproxy.option.logging" => inject_haproxy_defaults("option httplog"),
        _ if entry.disposition == Disposition::Externalized => {
            let argument = if matches!(entry.key.as_str(), "daemon" | "master-worker") {
                ""
            } else {
                " coverage"
            };
            format!("global\n  {}{argument}\n{}", entry.key, haproxy_tcp_base())
        }
        "directive.haproxy.maxconn.proxy-tcp"
        | "directive.haproxy.mode.tcp"
        | "directive.haproxy.bind.tcp"
        | "directive.haproxy.default-backend.tcp"
        | "directive.haproxy.balance.roundrobin-tcp"
        | "directive.haproxy.server.static-ip-tcp"
        | "directive.haproxy.retries.zero-tcp"
        | "directive.haproxy.timeout" => haproxy_tcp_base(),
        "directive.haproxy.maxconn.proxy-http"
        | "directive.haproxy.mode.http"
        | "directive.haproxy.bind.http"
        | "directive.haproxy.default-backend.http"
        | "directive.haproxy.balance.roundrobin-http"
        | "directive.haproxy.server.static-ip-http"
        | "directive.haproxy.retries.zero-http" => haproxy_http_base(),
        id => panic!("HAProxy manifest form has no executable probe: {id}"),
    }
}

fn haproxy_transport_probe(id: &str) -> Option<String> {
    match id {
        "directive.haproxy.bind.unix-tcp" => {
            Some(haproxy_tcp_base().replace("bind 127.0.0.1:15432", "bind /run/coverage.sock"))
        }
        "directive.haproxy.bind.unix-http" => {
            Some(haproxy_http_base().replace("bind 127.0.0.1:18080", "bind /run/coverage.sock"))
        }
        "directive.haproxy.balance.leastconn-tcp" => {
            Some(haproxy_tcp_base().replace("balance roundrobin", "balance leastconn"))
        }
        "directive.haproxy.balance.leastconn-http" => Some(
            inject_haproxy_http_defaults("option http-server-close")
                .replace("balance roundrobin", "balance leastconn"),
        ),
        "directive.haproxy.balance.first-tcp" => {
            Some(haproxy_tcp_base().replace("balance roundrobin", "balance first"))
        }
        "directive.haproxy.balance.first-http" => Some(
            inject_haproxy_http_defaults("option http-server-close")
                .replace("balance roundrobin", "balance first"),
        ),
        "directive.haproxy.server.dns-tcp" => Some(haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary database.lan:5432",
        )),
        "directive.haproxy.server.dns-http" => Some(haproxy_http_base().replace(
            "server api-1 127.0.0.1:3001",
            "server api-1 api.internal.lan:3001",
        )),
        "directive.haproxy.server.unix-tcp" => Some(haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary /run/database.sock",
        )),
        "directive.haproxy.server.unix-http" => Some(
            haproxy_http_base()
                .replace("server api-1 127.0.0.1:3001", "server api-1 /run/api.sock"),
        ),
        _ => None,
    }
}

fn haproxy_tcp_base() -> String {
    "defaults tcp_defaults\n  mode tcp\n  retries 0\n  timeout connect 10s\n  timeout client 5m\n  timeout server 5m\nfrontend database\n  bind 127.0.0.1:15432\n  maxconn 1000\n  default_backend database_pool\nbackend database_pool\n  balance roundrobin\n  server primary 127.0.0.1:5432\n".into()
}

fn haproxy_http_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18080\n  maxconn 100\n  acl api path_beg /api\n  use_backend api if api\n  default_backend fallback\nbackend api\n  balance roundrobin\n  server api-1 127.0.0.1:3001\nbackend fallback\n  balance roundrobin\n  server fallback-1 127.0.0.1:3002\n".into()
}

fn haproxy_strict_http_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18080\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app-1 127.0.0.1:3001\n".into()
}

fn haproxy_terminal_return_base() -> String {
    "frontend health\n  mode http\n  bind 127.0.0.1:18081\n  maxconn 100\n  http-request return status 200 content-type text/plain string healthy\n".into()
}

fn haproxy_terminal_redirect_base() -> String {
    "frontend redirect\n  mode http\n  bind 127.0.0.1:18082\n  maxconn 100\n  http-request redirect location https://example.test/new code 308\n".into()
}

fn haproxy_header_mutation_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18083\n  maxconn 100\n  http-request set-header X-Client-IP %[src]\n  http-request del-header X-Remove\n  default_backend app\nbackend app\n  balance roundrobin\n  http-response set-header X-Frame-Options same-origin\n  http-response del-header X-Powered-By\n  server app1 127.0.0.1:3000\n".into()
}

fn haproxy_tcp_listen_base() -> String {
    "defaults tcp_defaults\n  mode tcp\n  retries 0\n  timeout connect 10s\n  timeout client 5m\n  timeout server 5m\nlisten database\n  bind 127.0.0.1:15432\n  maxconn 1000\n  balance roundrobin\n  server primary 127.0.0.1:5432\nbackend database_pool\n  mode tcp\n  balance roundrobin\n  server secondary 127.0.0.1:5433\n".into()
}

fn haproxy_http_listen_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nlisten public\n  bind 127.0.0.1:18080\n  maxconn 100\n  balance roundrobin\n  server local 127.0.0.1:3001\nbackend database_pool\n  mode http\n  balance roundrobin\n  server primary 127.0.0.1:3002\n".into()
}

fn inject_haproxy_defaults(directive: &str) -> String {
    haproxy_tcp_base().replacen(
        "defaults tcp_defaults\n",
        &format!("defaults tcp_defaults\n  {directive}\n"),
        1,
    )
}

fn inject_haproxy_http_defaults(directive: &str) -> String {
    haproxy_http_base().replacen(
        "defaults web\n",
        &format!("defaults web\n  {directive}\n"),
        1,
    )
}
