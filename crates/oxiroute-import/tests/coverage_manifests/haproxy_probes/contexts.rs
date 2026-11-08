#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn assert_haproxy_lowered_subset(entries: &[DirectiveForm]) {
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
        "directive.haproxy.server.weighted-tcp",
        "directive.haproxy.server.health-options",
        "directive.haproxy.default-server.health-options",
        "directive.haproxy.default-server.weighted-tcp",
        "directive.haproxy.server.passive-health",
        "directive.haproxy.default-server.passive-health",
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
        "directive.haproxy.server.weighted-http",
        "directive.haproxy.default-server.weighted-http",
        "directive.haproxy.retries.zero-http",
        "directive.haproxy.retry-on.none",
        "directive.haproxy.retry-on.triggers",
        "directive.haproxy.retry-on.statuses",
        "directive.haproxy.retry-on.all",
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

pub(super) fn haproxy_parser_registered_sections() -> BTreeSet<String> {
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

pub(super) const fn haproxy_section_keyword(kind: SectionKind) -> &'static str {
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

pub(super) fn execute_haproxy_probe(entry: &DirectiveForm) -> HaproxyProbeReports {
    let source = haproxy_probe_source(entry);
    let parsed = parse_haproxy_source("coverage-probe.cfg", source.as_bytes());
    let resolved = resolve_parsed(parsed.clone());
    let lowered = import_haproxy(parsed.clone());
    (parsed, resolved, lowered)
}

pub(super) fn assert_haproxy_entry_probe(
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
                lowered
                    .value()
                    .validated()
                    .map(oxiroute_config::ValidatedConfig::as_draft)
                    .is_some(),
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
                    lowered
                        .value()
                        .validated()
                        .map(oxiroute_config::ValidatedConfig::as_draft)
                        .is_none(),
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
                lowered
                    .value()
                    .validated()
                    .map(oxiroute_config::ValidatedConfig::as_draft)
                    .is_some(),
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
pub(super) fn haproxy_context_directives(
    entry: &DirectiveForm,
    context: &str,
) -> Vec<&'static str> {
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
            vec!["server primary database.test:5432"]
        }
        "directive.haproxy.server.unix-tcp" | "directive.haproxy.server.unix-http" => {
            vec!["server primary /run/database.sock"]
        }
        "directive.haproxy.server.weighted-tcp" | "directive.haproxy.server.weighted-http" => {
            vec!["server primary 127.0.0.1:5432 weight 50"]
        }
        "directive.haproxy.server.health-options" => {
            vec!["server primary 127.0.0.1:5432 check inter 2s rise 2 fall 3"]
        }
        "directive.haproxy.default-server.health-options" => {
            vec!["default-server check inter 2s fastinter 1s downinter 5s rise 2 fall 3"]
        }
        "directive.haproxy.default-server.weighted-tcp"
        | "directive.haproxy.default-server.weighted-http" => {
            vec!["default-server weight 25"]
        }
        "directive.haproxy.server.passive-health" => {
            vec![
                "server primary 127.0.0.1:5432 check observe layer4 error-limit 5 on-error mark-down",
            ]
        }
        "directive.haproxy.default-server.passive-health" => {
            vec!["default-server check observe layer4 error-limit 5 on-error mark-down"]
        }
        "directive.haproxy.retries.zero-tcp" | "directive.haproxy.retries.zero-http" => {
            vec!["retries 0"]
        }
        "directive.haproxy.retries.positive" => vec!["retries 3"],
        "directive.haproxy.retry-on.none" => vec!["retry-on none"],
        "directive.haproxy.retry-on.triggers" => {
            vec!["retry-on conn-failure empty-response response-timeout junk-response"]
        }
        "directive.haproxy.retry-on.statuses" => vec!["retry-on 500 502 504"],
        "directive.haproxy.retry-on.all" => vec!["retry-on all"],
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

pub(super) fn haproxy_process_context_directives(
    id: &str,
    context: &str,
) -> Option<Vec<&'static str>> {
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

pub(super) fn haproxy_context_probe_source(
    entry: &DirectiveForm,
    context: &str,
    directive: &str,
) -> String {
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

pub(super) fn haproxy_http_check_send_probe_source(context: &str, directive: &str) -> String {
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

pub(super) fn inject_haproxy_context(
    source: &str,
    context: &str,
    key: &str,
    directive: &str,
) -> String {
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

pub(super) fn assert_haproxy_parsed_context(
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
