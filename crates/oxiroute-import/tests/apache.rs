#![cfg(unix)]

use std::{fs, net::SocketAddr, path::Path};

use oxiroute_config::{
    CertificateSource, HttpHostSelector, HttpPathSelector, HttpRouteAction, ListenerBind,
    UpstreamAlgorithm,
};
use oxiroute_import::{
    DiagnosticStage, ProvenanceRole, SourceFile, SourceId,
    apache::{
        E_DIRECTORY_MERGE, E_DYNAMIC_PROXY_PASS, E_REWRITE_UNSUPPORTED, E_UNSUPPORTED_DIRECTIVE,
        IncludeCandidateStatus, import_root, load, parse,
    },
};
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

    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Apache candidate");
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
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Apache balancer candidate");
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
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Apache TLS candidate");
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
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_REWRITE_UNSUPPORTED && diagnostic.stage() == DiagnosticStage::Resolve
    }));
}

#[test]
fn parser_retains_nested_directives_and_continuation_words() {
    let source = SourceFile::from_path(
        SourceId::new(0),
        Path::new("httpd.conf").to_path_buf(),
        b"<VirtualHost 127.0.0.1:8080>\n  ProxyPass / http://origin.example.test/ \\\n  # continued\n</VirtualHost>\n".to_vec(),
    );
    let document = parse(&source).value().clone();

    assert_eq!(document.directives.len(), 1);
    assert_eq!(document.directives[0].name.value, b"VirtualHost");
    let children = document.directives[0]
        .children
        .as_ref()
        .expect("VirtualHost children");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].name.value, b"ProxyPass");
    assert_eq!(children[0].arguments[0].value, b"/");
    assert_eq!(
        children[0].arguments[1].value,
        b"http://origin.example.test/"
    );
    assert!(children[0].line_span.range().end() > children[0].span.range().start());
}

#[test]
fn include_globs_are_byte_sorted_and_missing_optional_includes_are_silent() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    let snippets = directory.path().join("conf.d");
    fs::create_dir(&snippets).expect("Apache include directory");
    fs::write(
        &root,
        b"IncludeOptional missing.d/*.conf\nIncludeOptional conf.d/*.conf\n",
    )
    .expect("Apache root");
    fs::write(snippets.join("20-late.conf"), b"# late\n").expect("late include");
    fs::write(snippets.join("10-early.conf"), b"# early\n").expect("early include");

    let loaded = load(&root);
    assert!(!loaded.has_errors(), "{:#?}", loaded.diagnostics());
    let graph = loaded.value();
    assert_eq!(graph.sources.len(), 3);
    assert_eq!(
        graph
            .includes
            .iter()
            .map(|edge| edge.targets.clone())
            .collect::<Vec<_>>(),
        vec![Vec::new(), vec![SourceId::new(1), SourceId::new(2)],]
    );
    assert!(graph.includes[0].failure.is_none());
    assert!(graph.includes[0].optional);
    assert!(graph.includes[1].candidates.iter().all(|candidate| {
        candidate.status == IncludeCandidateStatus::Expanded(SourceId::new(1))
            || candidate.status == IncludeCandidateStatus::Expanded(SourceId::new(2))
    }));
    assert!(
        graph.sources[1]
            .canonical_path
            .file_name()
            .is_some_and(|name| name == "10-early.conf")
    );
    assert!(
        graph.sources[2]
            .canonical_path
            .file_name()
            .is_some_and(|name| name == "20-late.conf")
    );
}

#[test]
fn inherited_defaults_and_multi_address_vhosts_lower_with_include_provenance() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    let defaults = directory.path().join("defaults.conf");
    fs::write(&root, b"Include defaults.conf\nListen 0.0.0.0:8080\n<VirtualHost 127.0.0.1:8080 127.0.0.2:8080>\n</VirtualHost>\n")
        .expect("Apache root");
    fs::write(
        &defaults,
        b"ServerName APP.Example.Test:8080\nProxyPreserveHost On\nProxyPass / http://origin.example.test/\n",
    )
    .expect("Apache defaults");

    let report = import_root(&root);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Apache candidate");
    assert_eq!(config.listeners.len(), 1);
    assert!(matches!(
        config.listeners[0].bind,
        ListenerBind::Socket { address } if address == "0.0.0.0:8080".parse::<SocketAddr>().unwrap()
    ));
    let route = &config.http_services[0].routes[0];
    assert!(matches!(
        route.host,
        Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority { ref value })
            if value == "app.example.test:8080"
    ));
    let HttpRouteAction::Proxy { policy, .. } = &route.action else {
        panic!("inherited Apache route must proxy");
    };
    assert_eq!(
        policy.upstream_host,
        oxiroute_config::HttpUpstreamHost::PreserveIncoming
    );
    assert!(report.candidate.provenance.iter().any(|entry| {
        entry.path == "/http_services/0/routes/0"
            && entry.origins.iter().any(|origin| {
                origin.role == ProvenanceRole::Inherited
                    && origin.include_stack.len() == 1
                    && origin.path == fs::canonicalize(&defaults).unwrap()
            })
    }));
}

#[test]
fn inherited_tls_defaults_lower_with_value_provenance() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    let defaults = directory.path().join("tls-defaults.conf");
    fs::write(&root, b"Include tls-defaults.conf\nListen 127.0.0.1:8443\n<VirtualHost 127.0.0.1:8443>\n</VirtualHost>\n")
        .expect("Apache root");
    fs::write(
        &defaults,
        b"ServerName secure.example.test:8443\nSSLEngine On\nSSLCertificateFile /etc/ssl/certs/secure.pem\nSSLCertificateKeyFile /etc/ssl/private/secure.key\nProxyPass / https://origin.example.test/\n",
    )
    .expect("Apache TLS defaults");

    let report = import_root(&root);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Apache TLS candidate");
    assert_eq!(config.certificates.len(), 1);
    assert!(config.listeners[0].tls_profile.is_some());
    assert!(report.candidate.provenance.iter().any(|entry| {
        entry.path == "/certificates/0/source/certificate_chain_path"
            && entry.origins.iter().any(|origin| {
                origin.role == ProvenanceRole::Inherited && origin.include_stack.len() == 1
            })
    }));
}

#[test]
fn apache_host_case_keeps_explicit_port_without_widening_the_authority() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:8080\n<VirtualHost 127.0.0.1:8080>\n  ServerName APP.Example.Test:8080\n  ProxyPass / http://origin.example.test/\n</VirtualHost>\n",
    )
    .expect("Apache host case");

    let report = import_root(&root);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let route = &report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Apache candidate")
        .http_services[0]
        .routes[0];
    assert!(matches!(
        route.host,
        Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority { ref value })
            if value == "app.example.test:8080"
    ));
}

#[test]
fn ordered_proxy_passes_finalize_only_when_first_match_is_runtime_equivalent() {
    let directory = tempdir().expect("Apache fixture directory");
    let safe = directory.path().join("safe.conf");
    fs::write(
        &safe,
        b"Listen 127.0.0.1:8080\n<VirtualHost 127.0.0.1:8080>\n  ServerName app.example.test\n  ProxyPass /api http://api.example.test/api\n  ProxyPass / http://web.example.test/\n</VirtualHost>\n",
    )
    .expect("safe ProxyPass source");
    let safe_report = import_root(&safe);
    assert!(!safe_report.has_errors(), "{:#?}", safe_report.diagnostics);
    let routes = &safe_report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("safe candidate")
        .http_services[0]
        .routes;
    let named_paths = routes
        .iter()
        .filter(|route| route.host.is_some())
        .map(|route| route.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(named_paths.len(), 2);
    assert_eq!(
        named_paths[0],
        HttpPathSelector::RawPrefix {
            value: "/api".into()
        }
    );
    assert_eq!(
        named_paths[1],
        HttpPathSelector::RawPrefix { value: "/".into() }
    );

    let unsafe_source = directory.path().join("unsafe.conf");
    fs::write(
        &unsafe_source,
        b"Listen 127.0.0.1:8081\n<VirtualHost 127.0.0.1:8081>\n  ServerName app.example.test\n  ProxyPass / http://web.example.test/\n  ProxyPass /api http://api.example.test/api\n</VirtualHost>\n",
    )
    .expect("unsafe ProxyPass source");
    let unsafe_report = import_root(&unsafe_source);
    assert!(unsafe_report.has_errors());
    assert!(
        unsafe_report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(unsafe_report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == oxiroute_import::E_SEMANTICS_NOT_REPRESENTABLE
            && diagnostic.message().contains("first-match")
    }));
}

#[test]
fn static_balancer_member_policy_rejects_weight_and_runtime_options() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:8080\n<Proxy balancer://app>\n  BalancerMember http://origin.example.test loadfactor=2\n</Proxy>\n<VirtualHost 127.0.0.1:8080>\n  ServerName app.example.test\n  ProxyPass / balancer://app/\n</VirtualHost>\n",
    )
    .expect("Apache balancer policy");

    let report = import_root(&root);
    assert!(report.has_errors());
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == oxiroute_import::E_SEMANTICS_NOT_REPRESENTABLE
            && diagnostic.message().contains("BalancerMember")
    }));
}

#[test]
fn directory_scripts_auth_and_response_rewriting_fail_closed() {
    let directory = tempdir().expect("Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:8080\nScriptAlias /cgi-bin/ /srv/cgi-bin/\n<VirtualHost 127.0.0.1:8080>\n  ServerName app.example.test\n  <Directory /srv/cgi-bin>\n    Require all granted\n  </Directory>\n  ProxyPass / http://origin.example.test/\n  ProxyPassReverse / http://origin.example.test/\n</VirtualHost>\n",
    )
    .expect("Apache unsupported behavior");

    let report = import_root(&root);
    assert!(report.has_errors());
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == E_DIRECTORY_MERGE)
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_UNSUPPORTED_DIRECTIVE || diagnostic.code() == E_DYNAMIC_PROXY_PASS
    }));
}

#[test]
fn missing_root_is_reported_without_a_candidate() {
    let directory = tempdir().expect("Apache fixture directory");
    let report = import_root(Path::new(&directory.path().join("missing.conf")));

    assert!(report.has_errors());
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
}
