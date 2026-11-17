use std::{fs, path::Path};

use oxiroute_import::{DiagnosticStage, nginx::import_http_fragment};

use super::support::import_source;

#[test]
fn blocks_only_unsupported_nginx_behavior_without_emitting_partial_services() {
    let cases = [
        (
            "DNS upstream",
            "upstream backend { server backend.test:8080; }",
            "location / { proxy_pass http://backend; }",
            false,
        ),
        (
            "Unix upstream",
            "upstream backend { server unix:/run/backend.sock; }",
            "location / { proxy_pass http://backend; }",
            false,
        ),
        (
            "variable origin",
            "",
            "location / { proxy_pass http://$backend; }",
            true,
        ),
        (
            "insecure HTTPS",
            "upstream backend { server 127.0.0.1:8443; }",
            "location / { proxy_pass https://backend; }",
            false,
        ),
        (
            "header policy",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; proxy_set_header Host $host; }",
            false,
        ),
        (
            "authentication",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; auth_request /auth; }",
            true,
        ),
        (
            "cookie rewriting",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; proxy_cookie_path / /secure; }",
            false,
        ),
        (
            "buffering",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; proxy_buffering off; }",
            false,
        ),
        (
            "ambiguous path",
            "upstream backend { server 127.0.0.1:8080; }",
            "location /api/ { proxy_pass http://backend; }",
            true,
        ),
    ];

    for (label, upstream, location, blocked) in cases {
        let source = format!(
            "http {{ proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; {upstream} server {{ listen 127.0.0.1:8080 default_server; server_name test.example; {location} }} }}"
        );
        let report = import_source(&source);
        assert_eq!(
            report.has_errors(),
            blocked,
            "{label}: {:?}",
            report.diagnostics
        );
        assert_eq!(
            report.blocked_services.len(),
            usize::from(blocked),
            "{label}"
        );
        assert_eq!(
            report
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            blocked,
            "{label}"
        );
    }
}

#[test]
fn complete_nginx_configs_are_rejected_by_the_http_fragment_api() {
    let report = import_source(
        r"events {}
        http {
          server {
            listen 127.0.0.1:8088 default_server;
            location / { return 204; }
          }
        }",
    );

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Resolve
            && diagnostic.message().contains("not an HTTP fragment")
            && diagnostic.message().contains("only an http block")
    }));
}

#[test]
fn omitted_access_log_fails_closed_instead_of_disabling_nginx_default_logging() {
    let directory = tempfile::tempdir().expect("create source directory");
    fs::write(
        directory.path().join("nginx.conf"),
        "http { server { listen 127.0.0.1:8080 default_server; location / { return 204; } } }",
    )
    .expect("write source");

    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.message().contains("omitted nginx access_log")
            && diagnostic.message().contains("default combined log")
    }));
}

#[test]
fn blocks_implicit_nginx_proxy_defaults_and_unrepresented_tls_or_logging_policy() {
    let omitted = "proxy_ignore_headers";
    let mut policies = vec![
        ("proxy_http_version", "proxy_http_version 1.1;"),
        ("proxy_buffering", "proxy_buffering off;"),
        ("proxy_request_buffering", "proxy_request_buffering off;"),
        (
            "proxy_ignore_headers",
            "proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;",
        ),
    ];
    policies.retain(|(name, _)| *name != omitted);
    let source = format!(
        "http {{ {} upstream backend {{ server 127.0.0.1:8080; }} server {{ listen 127.0.0.1:8080 default_server; server_name test.example; location / {{ proxy_pass http://backend; }} }} }}",
        policies
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>()
            .join(" ")
    );
    let report = import_source(&source);
    assert!(report.has_errors(), "omitted {omitted}");
    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none(),
        "omitted {omitted}"
    );

    let report = import_source(
        "http { proxy_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; upstream backend { server 127.0.0.1:8080; } server { listen 127.0.0.1:8080 default_server; server_name test.example; location / { proxy_pass http://backend; } } }",
    );
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("modern nginx proxy and request-buffering defaults");
    assert!(config.http_services[0].routes[0].policy.request_buffering);

    for directive in [
        "access_log /var/log/nginx/access.log combined;",
        "ssl_session_cache off;",
        "ssl_session_cache none;",
        "ssl_session_cache builtin:1000;",
    ] {
        let source = format!(
            "http {{ proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; upstream backend {{ server 127.0.0.1:8080; }} server {{ listen 127.0.0.1:8443 ssl default_server; server_name test.example; ssl_certificate /etc/test.pem; ssl_certificate_key /etc/test.key; ssl_protocols TLSv1.2 TLSv1.3; {directive} location / {{ proxy_pass http://backend; }} }} }}"
        );
        let report = import_source(&source);
        assert!(report.has_errors(), "accepted {directive}");
        assert!(
            report
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "accepted {directive}"
        );
    }
}
