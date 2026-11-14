#![cfg(unix)]

use std::{fs, path::Path};

use oxiroute_config::ListenerBind;
use oxiroute_import::{
    Severity,
    nginx::{ImportReport, ListenEndpoint, import_http_fragment, load, resolve_http_fragment},
};
use tempfile::TempDir;

fn fixture_directory(name: &str) -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    let source = Path::new("tests/fixtures/nginx/synthetic/edge/ipv6-pair").join(name);
    fs::copy(source, directory.path().join("nginx.conf")).unwrap();
    directory
}

fn import_source(source: &str) -> ImportReport {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("nginx.conf"), source).unwrap();
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

fn binds(report: &ImportReport) -> Vec<String> {
    report
        .validated()
        .map_or_else(
            || panic!("not finalized: {:?}", report.diagnostics),
            oxiroute_config::ValidatedConfig::as_draft,
        )
        .listeners
        .iter()
        .map(|listener| match &listener.bind {
            ListenerBind::Socket { address, .. } => address.to_string(),
            other => panic!("unexpected bind {other:?}"),
        })
        .collect()
}

fn errors(report: &ImportReport) -> Vec<&str> {
    report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == Severity::Error)
        .map(oxiroute_import::Diagnostic::message)
        .collect()
}

#[test]
fn wildcard_and_ipv6_listen_pair_is_one_dual_stack_listener() {
    let directory = fixture_directory("nginx.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    assert_eq!(binds(&report), ["[::]:80"]);
    assert_eq!(report.summary().http_services, 1);
    let warnings = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == Severity::Warning)
        .map(|diagnostic| diagnostic.code().as_str())
        .collect::<Vec<_>>();
    assert_eq!(warnings, ["W_DUAL_STACK_LISTENER"]);
}

#[test]
fn pair_declared_by_different_server_sets_is_blocked() {
    let directory = fixture_directory("ipv4-only-server.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(report.validated().is_none());
    assert!(
        errors(&report)
            .iter()
            .any(|message| message.contains("different virtual servers"))
    );
    assert!(report.has_errors());
}

#[test]
fn ipv6_only_listen_is_blocked_with_a_clear_reason() {
    let report = import_source(
        "http { access_log off; server { listen [::]:80; server_name a.example.com; location / { return 204; } } }",
    );

    assert!(report.validated().is_none());
    assert_eq!(report.blocked_services.len(), 1);
    assert!(
        errors(&report)
            .iter()
            .any(|message| message.contains("IPv6-only nginx listen [::]"))
    );
}

#[test]
fn ipv4_wildcard_listens_stay_ipv4() {
    for listen in ["80", "*:80", "0.0.0.0:80"] {
        let report = import_source(&format!(
            "http {{ access_log off; server {{ listen {listen}; server_name a.example.com; location / {{ return 204; }} }} }}"
        ));

        assert_eq!(binds(&report), ["0.0.0.0:80"], "{listen}");
    }
}

#[test]
fn pair_with_a_distinct_ipv4_socket_on_the_same_port_still_overlaps() {
    let report = import_source(
        "http { access_log off; \
         server { listen 80; listen [::]:80; server_name a.example.com; location / { return 204; } } \
         server { listen 192.0.2.10:80; server_name b.example.com; location / { return 204; } } }",
    );

    assert!(report.has_errors());
    assert!(
        errors(&report)
            .iter()
            .any(|message| message.contains("overlap"))
    );
}

#[test]
fn wildcard_ipv4_and_ipv6_endpoints_stay_distinct_in_the_semantic_bind() {
    let directory = fixture_directory("nginx.conf");
    let loaded = load(Path::new("nginx.conf"), directory.path());
    let resolved = resolve_http_fragment(loaded);
    let http = &resolved.value().http_blocks[0];
    let bind = &http.binds[0];

    assert_eq!(http.binds.len(), 1);
    assert_eq!(
        bind.endpoint,
        ListenEndpoint::Socket {
            address: b"[::]".to_vec(),
            port: 80
        }
    );
    assert_eq!(
        bind.paired_endpoint,
        Some(ListenEndpoint::Socket {
            address: b"*".to_vec(),
            port: 80
        })
    );
    assert_eq!(bind.servers.len(), 3);
}
