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

fn socket_binds(report: &ImportReport) -> Vec<(String, bool)> {
    report
        .validated()
        .map_or_else(
            || panic!("not finalized: {:?}", report.diagnostics),
            oxiroute_config::ValidatedConfig::as_draft,
        )
        .listeners
        .iter()
        .map(|listener| match &listener.bind {
            ListenerBind::Socket { address, ipv6_only } => (address.to_string(), *ipv6_only),
            other => panic!("unexpected bind {other:?}"),
        })
        .collect()
}

#[test]
fn pair_declared_by_different_server_sets_is_two_listeners() {
    let directory = fixture_directory("ipv4-only-server.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    let binds = socket_binds(&report);
    assert_eq!(binds.len(), 2, "{binds:?}");
    assert!(
        binds.contains(&("0.0.0.0:80".to_owned(), false)),
        "{binds:?}"
    );
    assert!(binds.contains(&("[::]:80".to_owned(), true)), "{binds:?}");
    let draft = report.validated().unwrap().as_draft();
    let services = |address: &str| {
        let listener = draft
            .listeners
            .iter()
            .find(|listener| listener.bind.to_string() == address)
            .unwrap();
        let service = listener.service.as_deref().unwrap();
        draft
            .http_services
            .iter()
            .find(|candidate| candidate.name == service)
            .unwrap()
            .routes
            .len()
    };
    assert_eq!(services("0.0.0.0:80"), services("[::]:80") + 1);
}

#[test]
fn ipv6_only_listen_is_an_ipv6_only_listener() {
    let report = import_source(
        "http { access_log off; server { listen [::]:80; server_name a.example.com; location / { return 204; } } }",
    );

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    assert_eq!(socket_binds(&report), [("[::]:80".to_owned(), true)]);
}

#[test]
fn pairs_that_differ_in_ssl_or_default_server_stay_two_listeners() {
    for (ipv4, ipv6) in [
        ("80", "[::]:80 default_server"),
        ("80 default_server", "[::]:80"),
    ] {
        let report = import_source(&format!(
            "http {{ access_log off; \
             server {{ listen 80; listen [::]:80; server_name a.example.com; location / {{ return 204; }} }} \
             server {{ listen {ipv4}; listen {ipv6}; server_name b.example.com; location / {{ return 204; }} }} }}"
        ));
        assert!(
            !report.has_errors(),
            "{ipv4} {ipv6}: {:?}",
            report.diagnostics
        );
        assert_eq!(
            socket_binds(&report),
            [
                ("0.0.0.0:80".to_owned(), false),
                ("[::]:80".to_owned(), true)
            ],
            "{ipv4} {ipv6}"
        );
    }
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
