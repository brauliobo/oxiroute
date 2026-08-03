#![cfg(unix)]

use std::{fs, net::SocketAddr, path::Path};

use oxiroute_config::{
    CertificateSource, HttpPathSelector, HttpRouteAction, ListenerBind, UpstreamAlgorithm,
};
use oxiroute_import::{DiagnosticStage, apache::E_REWRITE_UNSUPPORTED, apache::import_root};
use tempfile::tempdir;

#[test]
fn imports_an_included_static_virtual_host_with_provenance() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    let snippet_dir = directory.path().join("conf.d");
    fs::create_dir(&snippet_dir).expect("Apache include directory");
    fs::write(&root, b"IncludeOptional conf.d/*.conf\n").expect("Apache root");
    fs::write(
        snippet_dir.join("site.conf"),
        b"Listen 127.0.0.1:8080\n<VirtualHost 127.0.0.1:8080>\n  ServerName app.example.test\n  ProxyPreserveHost On\n  ProxyPass / http://127.0.0.1:9000/\n</VirtualHost>\n",
    )
    .expect("Apache site");

    let report = import_root(&root);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(report.source_graph.sources.len(), 2);
    assert_eq!(report.source_graph.includes.len(), 1);
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len()
    );

    let config = report.candidate.config.as_ref().expect("Apache candidate");
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.http_services.len(), 1);
    assert!(matches!(
        config.listeners[0].bind,
        ListenerBind::Socket { address } if address == "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
    ));
    assert!(config.http_services[0].routes.iter().any(|route| {
        route.path == HttpPathSelector::RawPrefix { value: "/".into() }
            && matches!(route.action, HttpRouteAction::Proxy { .. })
    }));
    assert!(
        report
            .candidate
            .provenance
            .iter()
            .any(|entry| entry.path == "/listeners/0")
    );
    assert!(report.candidate.provenance.iter().any(|entry| {
        entry
            .origins
            .iter()
            .any(|origin| origin.include_stack.len() == 1)
    }));
}

#[test]
fn lowers_static_balancer_members_to_one_round_robin_pool() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:8080\n<Proxy balancer://app>\n  BalancerMember http://127.0.0.1:9001\n  BalancerMember http://127.0.0.1:9002\n  ProxySet lbmethod=byrequests\n</Proxy>\n<VirtualHost 127.0.0.1:8080>\n  ServerName app.example.test\n  ProxyPass / balancer://app/\n</VirtualHost>\n",
    )
    .expect("Apache balancer fixture");

    let report = import_root(&root);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report.candidate.config.expect("Apache balancer candidate");
    assert_eq!(config.upstream_pools.len(), 1);
    assert_eq!(config.upstream_pools[0].servers.len(), 2);
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::RoundRobin
    );
}

#[test]
fn lowers_tls_paths_as_value_bearing_certificate_material() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:8443\n<VirtualHost 127.0.0.1:8443>\n  ServerName secure.example.test\n  SSLEngine On\n  SSLCertificateFile /etc/ssl/certs/secure.pem\n  SSLCertificateKeyFile /etc/ssl/private/secure.key\n  ProxyPass / https://origin.example.test/\n</VirtualHost>\n",
    )
    .expect("Apache TLS fixture");

    let report = import_root(&root);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report.candidate.config.expect("Apache TLS candidate");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.tls_profiles.len(), 1);
    assert!(config.listeners[0].tls_profile.is_some());
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::Files {
            ref certificate_chain_path,
            ref private_key_path
        } if certificate_chain_path == Path::new("/etc/ssl/certs/secure.pem")
            && private_key_path == Path::new("/etc/ssl/private/secure.key")
    ));
    assert_eq!(
        report
            .candidate
            .operational_overlays
            .iter()
            .filter(|overlay| overlay.satisfied)
            .count(),
        2
    );
}

#[test]
fn unsupported_rewrite_blocks_finalization() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:8080\n<VirtualHost 127.0.0.1:8080>\n  ServerName app.example.test\n  RewriteEngine On\n  ProxyPass / http://127.0.0.1:9000/\n</VirtualHost>\n",
    )
    .expect("Apache rewrite fixture");

    let report = import_root(&root);
    assert!(report.has_errors());
    assert!(report.candidate.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_REWRITE_UNSUPPORTED && diagnostic.stage() == DiagnosticStage::Resolve
    }));
}

#[test]
fn missing_root_is_reported_without_a_candidate() {
    let directory = tempdir().expect("Apache fixture directory");
    let report = import_root(Path::new(&directory.path().join("missing.conf")));

    assert!(report.has_errors());
    assert!(report.candidate.config.is_none());
}
