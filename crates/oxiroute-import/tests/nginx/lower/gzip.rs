use oxiroute_config::{HttpGzipMinimumVersion, HttpGzipPolicy};
use oxiroute_import::DiagnosticStage;

use super::support::import_source;

#[test]
fn lowers_global_gzip_and_log_semantics_but_blocks_mismatched_tls_policy() {
    for directive in ["gzip off;", "access_log off;"] {
        let source = format!(
            "http {{ proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; {directive} upstream backend {{ server 127.0.0.1:8080; }} server {{ listen 127.0.0.1:8080 default_server; server_name test.example; location / {{ proxy_pass http://backend; }} }} }}"
        );
        let report = import_source(&source);
        assert!(report.blocked_services.is_empty());
        assert!(
            report
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_some(),
            "{:?}",
            report.diagnostics
        );
    }

    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8443 ssl default_server;
            server_name one.example;
            ssl_certificate /etc/one-chain.pem;
            ssl_certificate_key /etc/one-key.pem;
            ssl_protocols TLSv1.2 TLSv1.3;
            ssl_prefer_server_ciphers on;
            location / { proxy_pass http://backend; }
          }
          server {
            listen 127.0.0.1:8443 ssl;
            server_name two.example;
            ssl_certificate /etc/two-chain.pem;
            ssl_certificate_key /etc/two-key.pem;
            ssl_protocols TLSv1.2 TLSv1.3;
            ssl_prefer_server_ciphers off;
            location / { proxy_pass http://backend; }
          }
        }",
    );
    assert_eq!(report.blocked_services.len(), 1);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("mismatched TLS")
    }));
}

#[test]
fn lowers_inherited_nginx_gzip_policy_with_provenance() {
    let report = import_source(
        r"http {
          gzip on;
          gzip_comp_level 6;
          gzip_min_length 64;
          gzip_http_version 1.0;
          gzip_proxied off;
          gzip_vary on;
          gzip_types text/plain application/json;
          server {
            listen 127.0.0.1:8080 default_server;
            server_name gzip.example;
            location / { return 204; }
          }
        }",
    );

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    assert_eq!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .http_services[0]
            .gzip,
        Some(HttpGzipPolicy {
            level: 6,
            content_types: vec![
                "text/html".into(),
                "text/plain".into(),
                "application/json".into(),
            ],
            min_length_bytes: 64,
            min_http_version: HttpGzipMinimumVersion::Http10,
            disable_on_via: true,
            vary: true,
        })
    );
    for (path, directive) in [
        ("/http_services/0/gzip/level", b"gzip_comp_level".as_slice()),
        ("/http_services/0/gzip/content_types", b"gzip_types"),
        ("/http_services/0/gzip/min_length_bytes", b"gzip_min_length"),
        (
            "/http_services/0/gzip/min_http_version",
            b"gzip_http_version",
        ),
        ("/http_services/0/gzip/disable_on_via", b"gzip_proxied"),
        ("/http_services/0/gzip/vary", b"gzip_vary"),
    ] {
        let occurrence = report
            .source_graph
            .expanded_occurrences
            .iter()
            .find(|occurrence| occurrence.directive.name.value == directive)
            .unwrap_or_else(|| panic!("missing source directive {directive:?}"))
            .id;
        let provenance = report
            .provenance
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("missing provenance for {path}"));
        assert_eq!(
            provenance
                .origins
                .iter()
                .map(|origin| origin.occurrence)
                .collect::<Vec<_>>(),
            [occurrence]
        );
    }
}

#[test]
fn nginx_gzip_uses_level_and_content_type_defaults() {
    let report = import_source(
        r"http {
          gzip on;
          server {
            listen 127.0.0.1:8080 default_server;
            location / { return 204; }
          }
        }",
    );

    assert_eq!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .http_services[0]
            .gzip,
        Some(HttpGzipPolicy {
            level: 1,
            content_types: vec!["text/html".into()],
            min_length_bytes: 20,
            min_http_version: HttpGzipMinimumVersion::Http11,
            disable_on_via: true,
            vary: false,
        })
    );
}

#[test]
fn nginx_gzip_types_include_text_html_once_and_deduplicate_values() {
    let report = import_source(
        r"http {
          gzip on;
          gzip_types text/html text/plain text/html TEXT/PLAIN;
          server {
            listen 127.0.0.1:8080 default_server;
            location / { return 204; }
          }
        }",
    );

    assert_eq!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .http_services[0]
            .gzip
            .as_ref()
            .unwrap()
            .content_types,
        ["text/html", "text/plain"]
    );
}

#[test]
fn nginx_gzip_off_remains_disabled() {
    let report = import_source(
        r"http {
          gzip off;
          gzip_comp_level 9;
          gzip_types application/json;
          server {
            listen 127.0.0.1:8080 default_server;
            location / { return 204; }
          }
        }",
    );

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    assert_eq!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .http_services[0]
            .gzip,
        None
    );
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| entry.path == "/http_services/0/gzip")
    );
}

#[test]
fn mismatched_participating_virtual_host_gzip_policy_blocks_the_bind() {
    let report = import_source(
        r"http {
          gzip on;
          server {
            listen 127.0.0.1:8080 default_server;
            server_name one.example;
            location / { return 204; }
          }
          server {
            listen 127.0.0.1:8080;
            server_name two.example;
            gzip off;
            location / { return 204; }
          }
        }",
    );
    assert_eq!(report.blocked_services.len(), 1);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("different effective gzip policies")
    }));

    let report = import_source(
        r"http {
          gzip on;
          server {
            listen 127.0.0.1:8080 default_server;
            server_name only.example;
            location / { return 204; }
          }
          server {
            listen 127.0.0.1:8080;
            server_name only.example;
            gzip off;
            location / { return 204; }
          }
        }",
    );
    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some(),
        "{:?}",
        report.diagnostics
    );
    assert_eq!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .http_services[0]
            .gzip
            .as_ref()
            .unwrap()
            .level,
        1
    );

    for override_policy in [
        "gzip_comp_level 2;",
        "gzip_types text/plain;",
        "gzip_min_length 21;",
        "gzip_http_version 1.0;",
        "gzip_vary on;",
    ] {
        let source = format!(
            r"http {{
              gzip on;
              server {{
                listen 127.0.0.1:8080 default_server;
                server_name one.example;
                location / {{ return 204; }}
              }}
              server {{
                listen 127.0.0.1:8080;
                server_name two.example;
                {override_policy}
                location / {{ return 204; }}
              }}
            }}"
        );
        let report = import_source(&source);
        assert_eq!(
            report.blocked_services.len(),
            1,
            "accepted mismatched {override_policy}"
        );
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("different effective gzip policies")
        }));
    }
}

#[test]
fn rejects_invalid_or_unrepresentable_nginx_gzip_values() {
    for directive in [
        "gzip on; gzip_comp_level 0;",
        "gzip on; gzip_comp_level 10;",
        "gzip on; gzip_comp_level high;",
        "gzip maybe;",
        "gzip on; gzip_types *;",
        "gzip on; gzip_types invalid;",
        "gzip on; gzip_min_length invalid;",
        "gzip on; gzip_http_version 2;",
        "gzip on; gzip_proxied any;",
        "gzip on; gzip_vary maybe;",
    ] {
        let source = format!(
            "http {{ {directive} server {{ listen 127.0.0.1:8080 default_server; location / {{ return 204; }} }} }}"
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

#[test]
fn lowers_actual_shaped_fifteen_type_level_nine_gzip_policy() {
    let report = import_source(
        r"http {
          gzip on;
          gzip_comp_level 9;
          gzip_types
            text/css
            text/plain
            text/javascript
            application/javascript
            application/json
            application/x-javascript
            application/xml
            application/xml+rss
            application/xhtml+xml
            application/x-font-ttf
            application/x-font-opentype
            application/vnd.ms-fontobject
            image/svg+xml
            image/x-icon;
          server {
            listen 127.0.0.1:8080 default_server;
            location / { return 204; }
          }
        }",
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .unwrap();
    let gzip = config.http_services[0]
        .gzip
        .as_ref()
        .expect("enabled gzip policy");
    assert_eq!(gzip.level, 9);
    assert_eq!(gzip.content_types.len(), 15);
    assert_eq!(gzip.content_types[0], "text/html");
    assert_eq!(gzip.content_types[14], "image/x-icon");
    assert_eq!(gzip.min_length_bytes, 20);
    assert_eq!(gzip.min_http_version, HttpGzipMinimumVersion::Http11);
    assert!(gzip.disable_on_via);
    assert!(!gzip.vary);
}
