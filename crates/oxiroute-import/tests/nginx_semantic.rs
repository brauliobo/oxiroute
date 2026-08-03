#![cfg(unix)]

use std::{fmt::Write as _, fs, path::Path};

use oxiroute_import::{
    DiagnosticStage, E_DUPLICATE_IDENTITY, E_INCLUDE_NOT_FOUND, E_INVALID_VALUE, E_SOURCE_IO,
    E_UNSUPPORTED_FEATURE, Severity,
    nginx::{
        DefaultServerSelection, HttpDeclaration, HttpResolution, LocationKind,
        OccurrenceDisposition, ProxyPassScheme, ServerNameKind, SourceGraph, StaticEndpoint,
        UpstreamReference, load, resolve_http_fragment,
    },
};
use tempfile::TempDir;

#[test]
fn groups_many_virtual_servers_on_one_bind_and_selects_an_explicit_default() {
    let mut source = String::from("http {\n");
    for index in 0..16 {
        let default = if index == 11 { " default_server" } else { "" };
        writeln!(
            &mut source,
            "server {{ listen 80{default}; server_name host-{index}.example; }}"
        )
        .expect("write fixture string");
    }
    source.push_str("}\n");
    let resolved = resolve_source(source.as_bytes(), &[]);

    assert!(resolved.diagnostics().is_empty());
    let http = &resolved.value().http_blocks[0];
    assert_eq!(http.servers.len(), 16);
    assert_eq!(http.binds.len(), 1);
    let bind = &http.binds[0];
    assert_eq!(bind.servers.len(), 16);
    assert_eq!(bind.default_server, http.servers[11].origin.occurrence);
    assert_eq!(
        bind.default_selection,
        DefaultServerSelection::Explicit {
            listen: http.servers[11].listens[0].origin.occurrence,
        }
    );
}

#[test]
fn first_server_is_the_default_when_no_listen_is_explicitly_default() {
    let source = br"
        http {
            server { listen 127.0.0.1:8080; server_name first.example; }
            server { listen 127.0.0.1:8080; server_name second.example; }
        }
    ";
    let resolved = resolve_source(source, &[]);

    assert!(resolved.diagnostics().is_empty());
    let http = &resolved.value().http_blocks[0];
    assert_eq!(
        http.binds[0].default_server,
        http.servers[0].origin.occurrence
    );
    assert_eq!(
        http.binds[0].default_selection,
        DefaultServerSelection::First
    );
}

#[test]
fn classifies_exact_and_nginx_wildcard_server_names_without_losing_raw_bytes() {
    let source = br#"
        http {
            server {
                listen 80;
                server_name "Exact.Example" *.example.test .both.example api.*;
            }
        }
    "#;
    let resolved = resolve_source(source, &[]);

    assert!(resolved.diagnostics().is_empty());
    let names = &resolved.value().http_blocks[0].servers[0].server_names;
    assert_eq!(
        names.iter().map(|name| name.kind).collect::<Vec<_>>(),
        [
            ServerNameKind::Exact,
            ServerNameKind::LeadingWildcard,
            ServerNameKind::LeadingWildcardAndExact,
            ServerNameKind::TrailingWildcard,
        ]
    );
    assert_eq!(names[0].normalized, b"exact.example");
    assert_eq!(names[0].value.value, b"Exact.Example");
    assert_eq!(names[0].value.raw, br#""Exact.Example""#);
}

#[test]
fn accumulates_list_directives_and_inherits_the_proxy_pass_scalar_with_uri_shape() {
    let source = br"
        http {
            upstream backend {
                server 127.0.0.1:8080;
            }
            server {
                listen 80;
                listen 8080;
                server_name one.example;
                server_name two.example three.example;
                location /api/ {
                    proxy_pass http://backend/v1/;
                    location /api/private/ { }
                }
                location = /health {
                    proxy_pass http://backend;
                }
            }
        }
    ";
    let resolved = resolve_source(source, &[]);

    assert!(resolved.diagnostics().is_empty());
    let server = &resolved.value().http_blocks[0].servers[0];
    assert_eq!(server.listens.len(), 2);
    assert_eq!(server.server_names.len(), 3);
    assert_eq!(server.locations[0].kind, LocationKind::Prefix);
    let parent_proxy = server.locations[0]
        .proxy_pass
        .as_ref()
        .expect("declared parent proxy_pass");
    assert_eq!(parent_proxy.scheme, ProxyPassScheme::Http);
    assert_eq!(
        parent_proxy.replacement_uri.as_deref(),
        Some(b"/v1/".as_slice())
    );
    let nested = &server.locations[0].children[0];
    assert!(nested.proxy_pass_inherited);
    assert_eq!(
        nested
            .proxy_pass
            .as_ref()
            .expect("inherited proxy_pass")
            .origin,
        parent_proxy.origin
    );
    assert_eq!(server.locations[1].kind, LocationKind::Exact);
    assert_eq!(
        server.locations[1]
            .proxy_pass
            .as_ref()
            .expect("exact location proxy_pass")
            .replacement_uri,
        None
    );
}

#[test]
fn resolves_forward_upstream_references_and_preserves_declaration_and_endpoint_order() {
    let source = br"
        http {
            server {
                listen 80;
                server_name ordered.example;
                location / { proxy_pass http://later/base; }
            }
            upstream later {
                server 10.0.0.2:8082;
                server 10.0.0.1:8081;
            }
        }
    ";
    let resolved = resolve_source(source, &[]);

    assert!(resolved.diagnostics().is_empty());
    let http = &resolved.value().http_blocks[0];
    assert_eq!(
        http.declaration_order,
        [
            HttpDeclaration::Server(http.servers[0].origin.occurrence),
            HttpDeclaration::Upstream(http.upstreams[0].origin.occurrence),
        ]
    );
    assert_eq!(
        http.upstreams[0]
            .servers
            .iter()
            .map(
                |server| match server.endpoint.as_ref().expect("static endpoint") {
                    StaticEndpoint::Socket { address } => address.port(),
                    StaticEndpoint::Dns { port, .. } => *port,
                    StaticEndpoint::Unix { .. } => panic!("expected network endpoint"),
                }
            )
            .collect::<Vec<_>>(),
        [8082, 8081]
    );
    assert_eq!(
        http.servers[0].locations[0]
            .proxy_pass
            .as_ref()
            .expect("proxy_pass")
            .upstream,
        UpstreamReference::Resolved(http.upstreams[0].origin.occurrence)
    );
}

#[test]
fn retains_dns_and_unix_upstream_endpoints_without_resolution_or_substitution() {
    let source = br"
        http {
            upstream mixed {
                server app.internal:8080;
                server unix:/run/app.sock;
            }
            server {
                listen 127.0.0.1:8088;
                location / { proxy_pass http://mixed; }
            }
        }
    ";
    let resolved = resolve_source(source, &[]);

    assert!(resolved.diagnostics().is_empty());
    let endpoints = &resolved.value().http_blocks[0].upstreams[0].servers;
    assert!(matches!(
        endpoints[0].endpoint,
        Some(StaticEndpoint::Dns { ref host, port: 8080 }) if host == "app.internal"
    ));
    assert!(matches!(
        endpoints[1].endpoint,
        Some(StaticEndpoint::Unix { ref path }) if path == Path::new("/run/app.sock")
    ));
}

#[test]
fn represents_a_static_dns_proxy_pass_as_a_direct_endpoint() {
    let source = br"
        http {
            server {
                listen 80;
                server_name unresolved.example;
                location / { proxy_pass http://missing/path/; }
            }
        }
    ";
    let resolved = resolve_source(source, &[]);
    let proxy = resolved.value().http_blocks[0].servers[0].locations[0]
        .proxy_pass
        .as_ref()
        .expect("represented unresolved proxy_pass");

    assert_eq!(proxy.upstream, UpstreamReference::Direct);
    assert_eq!(proxy.authority, b"missing");
    assert_eq!(proxy.replacement_uri.as_deref(), Some(b"/path/".as_slice()));
    assert!(matches!(
        proxy.direct_endpoint.as_ref(),
        Some(StaticEndpoint::Dns { host, port: 80 }) if host == "missing"
    ));
    assert!(resolved.diagnostics().is_empty());
}

#[test]
fn represents_a_static_numeric_proxy_pass_as_a_direct_endpoint() {
    let source = br"
        http {
            server {
                listen 80;
                server_name direct.example;
                location / { proxy_pass http://127.0.0.1:8080/base/; }
            }
        }
    ";
    let resolved = resolve_source(source, &[]);
    let proxy = resolved.value().http_blocks[0].servers[0].locations[0]
        .proxy_pass
        .as_ref()
        .expect("represented direct proxy_pass");

    assert!(resolved.diagnostics().is_empty());
    assert_eq!(proxy.upstream, UpstreamReference::Direct);
    assert!(matches!(
        proxy.direct_endpoint.as_ref(),
        Some(StaticEndpoint::Socket { address })
            if *address == "127.0.0.1:8080".parse().unwrap()
    ));
    assert_eq!(proxy.replacement_uri.as_deref(), Some(b"/base/".as_slice()));
}

#[test]
fn duplicate_server_names_warn_while_other_duplicate_identities_block() {
    let source = br"
        http {
            upstream duplicate {
                server 127.0.0.1:8080;
                server 127.0.0.1:8080;
            }
            upstream duplicate { server 127.0.0.1:8081; }
            server {
                listen 80 default_server;
                server_name duplicate.example;
            }
            server {
                listen 80 default_server;
                server_name duplicate.example;
            }
        }
    ";
    let resolved = resolve_source(source, &[]);

    assert_eq!(resolved.diagnostics().len(), 4);
    assert!(
        resolved
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code() == E_DUPLICATE_IDENTITY)
    );
    assert_eq!(
        resolved
            .value()
            .decisions
            .iter()
            .filter(|decision| {
                decision.disposition == OccurrenceDisposition::Blocking(E_DUPLICATE_IDENTITY)
            })
            .count(),
        3
    );
    assert_eq!(
        resolved
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity() == Severity::Warning)
            .count(),
        1
    );
}

#[test]
fn duplicate_scalar_directives_are_blocking_before_lowering() {
    let source = br"
        http {
            client_max_body_size 1m;
            client_max_body_size 2m;
            proxy_connect_timeout 1s;
            proxy_connect_timeout 2s;
            proxy_read_timeout 1s;
            proxy_read_timeout 2s;
            proxy_send_timeout 1s;
            proxy_send_timeout 2s;
            proxy_http_version 1.0;
            proxy_http_version 1.1;
            upstream one { server 127.0.0.1:8080; }
            upstream two { server 127.0.0.1:8081; }
            server {
                listen 127.0.0.1:8443 ssl default_server;
                server_name duplicate-scalars.example;
                ssl_protocols TLSv1.2;
                ssl_protocols TLSv1.3;
                http2 on;
                http2 off;
                location / {
                    proxy_pass http://one;
                    proxy_pass http://two;
                }
            }
        }
    ";
    let resolved = resolve_source(source, &[]);

    assert_eq!(
        resolved
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code() == E_DUPLICATE_IDENTITY)
            .count(),
        8
    );
    assert_eq!(
        resolved
            .value()
            .decisions
            .iter()
            .filter(|decision| {
                decision.disposition == OccurrenceDisposition::Blocking(E_DUPLICATE_IDENTITY)
            })
            .count(),
        8
    );
}

#[test]
fn registers_keepalive_and_proxy_cookie_flag_policies_in_their_supported_contexts() {
    let resolved = resolve_source(
        br"
            http {
                keepalive_timeout 65s;
                proxy_cookie_flags session secure httponly samesite=lax;
                upstream backend { server 127.0.0.1:8080; }
                server {
                    listen 127.0.0.1:8088 default_server;
                    location / { proxy_pass http://backend; }
                }
            }
        ",
        &[],
    );

    assert!(
        resolved.diagnostics().is_empty(),
        "{:?}",
        resolved.diagnostics()
    );
    for name in [b"keepalive_timeout".as_slice(), b"proxy_cookie_flags"] {
        assert!(resolved.value().decisions.iter().any(|decision| {
            decision.name.value == name && decision.disposition == OccurrenceDisposition::Resolved
        }));
    }
}

#[test]
fn overlapping_listens_block_while_protocol_options_are_reconciled() {
    let overlapping = resolve_source(
        br"http {
                server { listen 0.0.0.0:8080; server_name wildcard.example; }
                server { listen 127.0.0.1:8080; server_name specific.example; }
            }",
        &[],
    );
    assert!(overlapping.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_INVALID_VALUE
            && diagnostic.stage() == DiagnosticStage::Resolve
            && diagnostic.severity() == Severity::Error
            && diagnostic.message().contains("conflicting listen")
    }));

    let protocols = resolve_source(
        br"http {
                server { listen 127.0.0.1:8443 ssl; server_name tls.example; }
                server { listen 127.0.0.1:8443; server_name clear.example; }
            }",
        &[],
    );
    assert!(protocols.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_INVALID_VALUE
            && diagnostic.stage() == DiagnosticStage::Resolve
            && diagnostic.severity() == Severity::Warning
            && diagnostic
                .message()
                .contains("protocol options are reconciled")
    }));
}

#[test]
fn failed_reachable_includes_have_blocking_decisions_with_source_spans() {
    let directory = tempdir();
    write(
        &directory.path().join("nginx.conf"),
        b"http { include missing.conf; include unreadable.conf; }",
    );
    fs::create_dir(directory.path().join("unreadable.conf")).expect("create unreadable include");
    let loaded = load(Path::new("nginx.conf"), directory.path());
    let graph = loaded.value().clone();
    let resolved = resolve_http_fragment(loaded);

    assert!(
        resolved
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == E_INCLUDE_NOT_FOUND)
    );
    assert!(
        resolved
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == E_SOURCE_IO)
    );
    let includes = resolved
        .value()
        .decisions
        .iter()
        .filter(|decision| decision.name.value == b"include")
        .collect::<Vec<_>>();
    assert_eq!(includes.len(), 2);
    assert_eq!(
        includes[0].disposition,
        OccurrenceDisposition::Blocking(E_INCLUDE_NOT_FOUND)
    );
    assert_eq!(
        includes[1].disposition,
        OccurrenceDisposition::Blocking(E_SOURCE_IO)
    );
    assert!(includes.iter().all(|decision| {
        decision.span.source() == graph.root.expect("root source")
            && !decision.span.range().is_empty()
    }));
}

#[test]
fn every_expanded_occurrence_has_one_ordered_terminal_decision() {
    let root = br"
        http {
            include site.conf;
            mystery top;
        }
    ";
    let included = br#"
        server {
            listen 80;
            server_name "Included.Example";
            unsupported value;
        }
    "#;
    let loaded = load_source(root, &[("site.conf", included)]);
    let graph = loaded.value().clone();
    let resolved = resolve_http_fragment(loaded);
    let decisions = &resolved.value().decisions;

    assert_eq!(decisions.len(), graph.expanded_occurrences.len());
    assert!(
        decisions
            .iter()
            .enumerate()
            .all(|(index, decision)| decision.occurrence.get() == index)
    );
    let include = decisions
        .iter()
        .find(|decision| decision.name.value == b"include")
        .expect("include decision");
    assert_eq!(include.disposition, OccurrenceDisposition::Structural);
    let included_name = decisions
        .iter()
        .find(|decision| decision.name.value == b"server_name")
        .expect("included server_name decision");
    assert_eq!(included_name.provenance.include_stack.len(), 1);
    assert_eq!(included_name.arguments[0].raw, br#""Included.Example""#);
    assert_eq!(
        decisions
            .iter()
            .filter(|decision| {
                decision.disposition == OccurrenceDisposition::Blocking(E_UNSUPPORTED_FEATURE)
            })
            .count(),
        2
    );
}

#[test]
fn represents_variable_special_location_and_static_https_forms() {
    let source = br"
        http {
            upstream backend { server 127.0.0.1:8080; }
            server {
                listen 80;
                server_name $host;
                location ~ ^/regex { proxy_pass http://backend; }
                location @named { proxy_pass http://backend; }
                location /variable { proxy_pass http://$backend; }
                location /tls { proxy_pass https://backend/; }
                location ^~ /images { proxy_pass http://backend; }
            }
        }
    ";
    let resolved = resolve_source(source, &[]);
    let server = &resolved.value().http_blocks[0].servers[0];

    assert_eq!(server.server_names[0].kind, ServerNameKind::Variable);
    assert_eq!(
        server
            .locations
            .iter()
            .map(|location| location.kind)
            .collect::<Vec<_>>(),
        [
            LocationKind::Regex,
            LocationKind::Named,
            LocationKind::Prefix,
            LocationKind::Prefix,
            LocationKind::PrefixNoRegex,
        ]
    );
    assert_eq!(
        server.locations[2]
            .proxy_pass
            .as_ref()
            .expect("variable proxy_pass")
            .upstream,
        UpstreamReference::Variable
    );
    assert_eq!(
        server.locations[3]
            .proxy_pass
            .as_ref()
            .expect("HTTPS proxy_pass")
            .scheme,
        ProxyPassScheme::Https
    );
    assert!(!resolved.diagnostics().is_empty());
    assert!(
        resolved
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code() == E_UNSUPPORTED_FEATURE)
    );
    assert_eq!(
        resolved
            .value()
            .decisions
            .iter()
            .filter(|decision| matches!(decision.disposition, OccurrenceDisposition::Blocking(_)))
            .count(),
        5
    );
}

fn load_source(root: &[u8], includes: &[(&str, &[u8])]) -> oxiroute_import::Report<SourceGraph> {
    let directory = tempdir();
    write(&directory.path().join("nginx.conf"), root);
    for (name, contents) in includes {
        write(&directory.path().join(name), contents);
    }
    load(Path::new("nginx.conf"), directory.path())
}

fn resolve_source(
    root: &[u8],
    includes: &[(&str, &[u8])],
) -> oxiroute_import::Report<HttpResolution> {
    resolve_http_fragment(load_source(root, includes))
}

fn write(path: &Path, contents: &[u8]) {
    fs::write(path, contents).expect("write nginx fixture");
}

fn tempdir() -> TempDir {
    tempfile::tempdir().expect("create tempdir")
}
