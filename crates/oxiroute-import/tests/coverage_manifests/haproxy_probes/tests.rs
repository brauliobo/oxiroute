#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn haproxy_exposes_only_diagnostic_carrying_resolution_and_import_entry_points() {
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
pub(super) fn haproxy_manifest_forms_execute_parser_semantic_and_lowering_decisions() {
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
pub(super) fn haproxy_manifest_contexts_and_http_matching_requirements_are_executable() {
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
        .validated()
        .expect("raw path finalizes")
        .as_draft();
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
        .validated()
        .expect("exact authority finalizes")
        .as_draft();
    assert!(matches!(
        host_config.http_services[0].routes[0].host,
        Some(HttpHostSelector::ExactAuthority { ref value }) if value == "api.example.test"
    ));

    let static_tcp = import_haproxy(parse_haproxy_source(
        "static-tcp.cfg",
        haproxy_tcp_base().as_bytes(),
    ));
    assert!(static_tcp.value().validated().is_some());

    let unix_listener_source =
        haproxy_tcp_base().replace("bind 127.0.0.1:15432", "bind /run/coverage.sock");
    let unix_listener = import_haproxy(parse_haproxy_source(
        "unix-listener.cfg",
        unix_listener_source.as_bytes(),
    ));
    assert!(matches!(
        unix_listener.value().validated().unwrap().as_draft().listeners[0].bind,
        ListenerBind::Unix { ref path, .. } if path == std::path::Path::new("/run/coverage.sock")
    ));

    let dns_source = haproxy_tcp_base().replace(
        "server primary 127.0.0.1:5432",
        "server primary database.internal:5432",
    );
    let dns = import_haproxy(parse_haproxy_source("dns.cfg", dns_source.as_bytes()));
    assert!(matches!(
        dns.value().validated().unwrap().as_draft().upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Dns { ref host, port: 5432 } if host == "database.internal"
    ));

    let unix_source = haproxy_tcp_base().replace(
        "server primary 127.0.0.1:5432",
        "server primary /run/database.sock",
    );
    let unix = import_haproxy(parse_haproxy_source("unix.cfg", unix_source.as_bytes()));
    assert!(matches!(
        unix.value().validated().unwrap().as_draft().upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Unix { ref path } if path == std::path::Path::new("/run/database.sock")
    ));

    let least_source = haproxy_tcp_base().replace("balance roundrobin", "balance leastconn");
    let least = import_haproxy(parse_haproxy_source("least.cfg", least_source.as_bytes()));
    assert_eq!(
        least.value().validated().unwrap().as_draft().upstream_pools[0].algorithm,
        UpstreamAlgorithm::LeastConnections
    );

    let unbounded_http = import_haproxy(parse_haproxy_source(
        "unbounded-http.cfg",
        haproxy_strict_http_base().as_bytes(),
    ));
    let http_config = unbounded_http
        .value()
        .validated()
        .expect("strict HTTP probe finalizes")
        .as_draft();
    assert_eq!(http_config.http_services[0].max_request_body_bytes, None);
}
