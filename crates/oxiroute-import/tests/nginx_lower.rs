#![cfg(unix)]

use std::{fmt::Write as _, fs, path::Path};

use oxiroute_config::{
    CertificateSource, HttpGzipMinimumVersion, HttpGzipPolicy, HttpHostSelector, HttpPathSelector,
    HttpRouteAction, HttpUpstreamHost, ListenerBind, UpstreamConnectionReuse, validate_config,
};
use oxiroute_import::{DiagnosticStage, nginx::import_http_fragment};
use tempfile::TempDir;

#[test]
fn fully_explicit_proxy_fixture_finalizes_with_canonical_routes() {
    let directory = fixture("representable.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    assert_eq!(report.source_graph.sources.len(), 1);
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len()
    );
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let config = report.config.as_ref().expect("finalized nginx config");
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(config.http_services[0].routes.len(), 5);
    assert!(config.http_services[0].routes.iter().any(|route| matches!(
        route.path,
        HttpPathSelector::RawPrefix { ref value } if value == "/api"
    )));
    assert!(config.http_services[0].routes.iter().any(|route| matches!(
        route.host,
        Some(HttpHostSelector::NormalizedHost { ref value }) if value == "other.example.test"
    )));
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert_eq!(policy.upstream_host, HttpUpstreamHost::PreserveIncoming);
    assert_eq!(policy.retry.max_retries, 0);
    let mut validated = config.clone();
    validate_config(&mut validated).expect("canonical validation");
    assert_eq!(&validated, config);
    for path in [
        "/listeners/0",
        "/http_services/0/routes/0/action/policy",
        "/upstream_pools/0/servers/0/endpoint/address",
    ] {
        assert!(
            report
                .provenance
                .iter()
                .any(|provenance| provenance.path == path)
        );
    }
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("private key material") })
    );
}

#[test]
fn lowers_non_root_nginx_raw_prefixes() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name default.example;
            location / { proxy_pass http://backend; }
            location /api { proxy_pass http://backend; }
          }
        }",
    );

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    assert!(
        report.config.as_ref().unwrap().http_services[0]
            .routes
            .iter()
            .any(|route| {
                route.path
                    == HttpPathSelector::RawPrefix {
                        value: "/api".into(),
                    }
            })
    );
}

#[test]
fn default_servers_also_require_a_representable_root_catch_all() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name default.example;
            location /api { proxy_pass http://backend; }
          }
        }",
    );

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("default server")
            && diagnostic.message().contains("fallback")
    }));
}

#[test]
fn lowers_explicit_nginx_proxy_version_with_canonical_defaults() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name proxy.example;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    assert_eq!(report.draft.listeners.len(), 1);
    assert_eq!(report.draft.http_services.len(), 1);
    let config = report.config.expect("canonical proxy defaults");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route");
    };
    assert_eq!(
        policy.upstream_host,
        HttpUpstreamHost::Literal {
            value: "backend".into()
        }
    );
    assert_eq!(
        config.upstream_pools[0].connection_reuse,
        UpstreamConnectionReuse::Never
    );
}

#[test]
fn default_server_retains_named_selectors_before_its_fallback() {
    let report = import_source(
        r"http {
          server {
            listen 127.0.0.1:8088 default_server;
            server_name api.example.test;
            location / { return 200 default; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name *.example.test;
            location / { return 200 wildcard; }
          }
        }",
    );

    let config = report.config.expect("default server host selectors");
    let routes = &config.http_services[0].routes;
    assert!(routes.iter().any(|route| {
        route.host
            == Some(HttpHostSelector::NormalizedHost {
                value: "api.example.test".into(),
            })
            && matches!(
                &route.action,
                HttpRouteAction::FixedResponse { body, .. } if body == "default"
            )
    }));
    assert!(routes.iter().any(|route| {
        route.host.is_none()
            && matches!(
                &route.action,
                HttpRouteAction::FixedResponse { body, .. } if body == "default"
            )
    }));
}

#[test]
fn blocks_non_default_servers_without_a_representable_local_catch_all() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name default.example;
            location / { proxy_pass http://backend; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name named.example;
            location /api {
              proxy_pass http://backend;
              location / { proxy_pass http://backend; }
            }
          }
        }",
    );

    assert_eq!(report.blocked_services.len(), 1);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("non-default server")
            && diagnostic.message().contains("fallback")
    }));
}

#[test]
fn lowers_nginx_leading_wildcards_without_widening_host_matching() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name default.example;
            location / { proxy_pass http://backend; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name *.example.test;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    assert!(
        report.config.as_ref().unwrap().http_services[0]
            .routes
            .iter()
            .any(|route| {
                route.host
                    == Some(HttpHostSelector::NginxLeadingWildcard {
                        value: "example.test".into(),
                    })
            })
    );
}

#[test]
fn lowering_uses_only_first_wins_exact_and_leading_dot_name_claims() {
    let mixed = import_source(
        r"http {
          server {
            listen 127.0.0.1:8088 default_server;
            server_name _;
            location / { return 204; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name mixed.example;
            location / { return 204; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name .mixed.example;
            location / { return 204; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name *.mixed.example;
            location / { return 204; }
          }
        }",
    );
    assert!(!mixed.has_errors(), "{:?}", mixed.diagnostics);
    let hosts = mixed.config.unwrap().http_services[0]
        .routes
        .iter()
        .filter_map(|route| route.host.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        hosts,
        [
            HttpHostSelector::NormalizedHost {
                value: "mixed.example".into(),
            },
            HttpHostSelector::NginxLeadingWildcard {
                value: "mixed.example".into(),
            },
        ]
    );

    let leading_dot_first = import_source(
        r"http {
          server {
            listen 127.0.0.1:8088 default_server;
            server_name _;
            location / { return 204; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name .first.example;
            location / { return 204; }
          }
          server {
            listen 127.0.0.1:8088;
            server_name first.example *.first.example;
            location / { return 204; }
          }
        }",
    );
    assert!(
        !leading_dot_first.has_errors(),
        "{:?}",
        leading_dot_first.diagnostics
    );
    let hosts = leading_dot_first.config.unwrap().http_services[0]
        .routes
        .iter()
        .filter_map(|route| route.host.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        hosts,
        [HttpHostSelector::NginxLeadingDot {
            value: "first.example".into(),
        }]
    );
}

#[test]
fn accepts_matching_secure_test_key_without_exposing_material() {
    let directory = fixture("representable.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("private key material") })
    );
}

#[test]
fn merges_one_certificate_lineage_across_distinct_tls_binds() {
    let directory = tempfile::tempdir().expect("create TLS source directory");
    let certificate =
        fs::canonicalize("tests/fixtures/nginx/proxy.pem").expect("canonical certificate fixture");
    let private_key = copy_secure_key(&directory, "proxy-key.pem", "proxy-key.pem");
    let source = format!(
        r"http {{
          access_log off;
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend {{ server 127.0.0.1:8080; }}
          server {{
            listen 127.0.0.1:8443 ssl default_server;
            server_name first.example.test;
            ssl_certificate {};
            ssl_certificate_key {};
            ssl_protocols TLSv1.2 TLSv1.3;
            location / {{ proxy_pass http://backend; }}
          }}
          server {{
            listen 127.0.0.1:8444 ssl default_server;
            server_name second.example.test;
            ssl_certificate {};
            ssl_certificate_key {};
            ssl_protocols TLSv1.2 TLSv1.3;
            location / {{ proxy_pass http://backend; }}
          }}
        }}",
        certificate.display(),
        private_key.display(),
        certificate.display(),
        private_key.display(),
    );
    fs::write(directory.path().join("nginx.conf"), source).expect("write TLS source");

    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
    assert!(report.blocked_services.is_empty());
    let config = report.config.as_ref().expect("distinct TLS listeners");
    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.certificates[0].dns_names.len(), 2);
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("private key material") })
    );
}

#[test]
fn lowers_certificate_paths_without_reading_operational_material() {
    let directory = tempfile::tempdir().expect("create TLS source directory");
    let source = fs::read_to_string("tests/fixtures/nginx/representable.conf")
        .expect("read representable TLS fixture")
        .replacen("http {", "http { access_log off;", 1)
        .replace("@CERTIFICATE@", "/definitely/missing/fullchain.pem")
        .replace("@PRIVATE_KEY@", "/definitely/missing/privkey.pem");
    fs::write(directory.path().join("nginx.conf"), source).expect("write TLS source");

    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
    let config = report.config.as_ref().expect("path-based TLS config");
    assert!(report.blocked_services.is_empty());
    assert_eq!(config.certificates[0].dns_names.len(), 2);
    assert_eq!(
        config.certificates[0].source,
        CertificateSource::Files {
            certificate_chain_path: "/definitely/missing/fullchain.pem".into(),
            private_key_path: "/definitely/missing/privkey.pem".into(),
        }
    );
    assert!(report.diagnostics.iter().all(|diagnostic| {
        !diagnostic.message().contains("certificate metadata")
            && !diagnostic.message().contains("private key material")
    }));
}

#[test]
fn lowers_exact_ip_server_names_as_canonical_certificate_identities_without_reading_files() {
    let report = import_source(
        r"http {
          server {
            listen 192.0.2.10:8443 ssl default_server;
            server_name 192.0.2.10;
            ssl_certificate /definitely/missing/shared-chain.pem;
            ssl_certificate_key /definitely/missing/shared-key.pem;
            ssl_protocols TLSv1.2 TLSv1.3;
            location / { return 204; }
          }
          server {
            listen [2001:db8::1]:8443 ssl default_server;
            server_name 2001:0DB8:0:0:0:0:0:1;
            ssl_certificate /definitely/missing/shared-chain.pem;
            ssl_certificate_key /definitely/missing/shared-key.pem;
            ssl_protocols TLSv1.2 TLSv1.3;
            location / { return 204; }
          }
        }",
    );

    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    let config = report.config.expect("IP-bound TLS listeners");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(
        config.certificates[0].dns_names,
        ["192.0.2.10", "2001:db8::1"]
    );
    assert_eq!(config.listeners.len(), 2);
    assert!(config.listeners.iter().all(|listener| {
        let profile = listener.tls_profile.as_ref().expect("TLS profile");
        config
            .tls_profiles
            .iter()
            .find(|candidate| &candidate.name == profile)
            .is_some_and(|profile| profile.default_certificate == config.certificates[0].name)
    }));
}

#[test]
fn finalizes_explicit_ipv6_proxy_topology() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server [2001:db8::20]:8080; }
          server {
            listen [::1]:8080 default_server;
            server_name ipv6.example;
            location / { proxy_pass http://backend; }
          }
          server {
            listen 0.0.0.0:8081 default_server;
            server_name wildcard.example;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(report.blocked_services.is_empty());
    assert_eq!(report.config.as_ref().unwrap().listeners.len(), 2);
}

#[test]
fn hostrouter_shaped_dns_service_finalizes_without_a_placeholder() {
    let directory = fixture("hostrouter-partial.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    assert_eq!(report.config.as_ref().unwrap().listeners.len(), 2);
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("static IP endpoint")
    }));
}

#[test]
fn unix_http_listener_is_retained_without_a_socket_placeholder() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server backend.internal:8080; }
          server {
            listen unix:/run/nginx/proxy.sock default_server;
            server_name proxy.example;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(report.blocked_services.is_empty());
    assert!(matches!(
        report.config.as_ref().unwrap().listeners[0].bind,
        ListenerBind::Unix { ref path, .. } if path == Path::new("/run/nginx/proxy.sock")
    ));
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("not an explicit socket or canonical Unix address")
    }));
}

#[test]
fn blocks_only_unsupported_nginx_behavior_without_emitting_partial_services() {
    let cases = [
        (
            "DNS upstream",
            "upstream backend { server backend.lan:8080; }",
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
        assert_eq!(report.config.is_none(), blocked, "{label}");
    }
}

#[test]
fn exact_location_fixed_and_redirect_actions_finalize_without_placeholders() {
    let report = import_source(
        r"http {
          server {
            listen 127.0.0.1:8088 default_server;
            server_name EXAMPLE.TEST;
            location = /health { return 204; }
            location = /old { return 308 https://example.test/new; }
            location / { return 404; }
          }
        }",
    );

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let config = report.config.as_ref().expect("fixed/static nginx config");
    assert!(matches!(
        config.http_services[0].routes[0].path,
        HttpPathSelector::Exact { ref value } if value == "/health"
    ));
    assert!(matches!(
        config.http_services[0].routes[0].action,
        HttpRouteAction::FixedResponse { status: 204, .. }
    ));
    assert!(
        config.http_services[0]
            .routes
            .iter()
            .any(|route| matches!(route.action, HttpRouteAction::Redirect { status: 308, .. }))
    );
    assert!(config.http_services[0].routes.iter().any(|route| matches!(
        route.action,
        HttpRouteAction::FixedResponse { status: 404, .. }
    )));
    assert!(config.upstream_pools.is_empty());
    assert!(report.provenance.iter().all(|provenance| {
        !provenance.path.contains("/action/upstream_pool")
            && !provenance.path.contains("/action/policy")
    }));
}

#[test]
fn lowers_static_index_behavior_into_canonical_static_routes() {
    for index in ["", "index home.html;"] {
        let report = import_source(&format!(
            "http {{ server {{ listen 127.0.0.1:8088 default_server; location / {{ root /srv/www; {index} }} }} }}"
        ));

        let config = report.config.as_ref().expect("static config");
        assert!(report.blocked_services.is_empty());
        assert!(report.draft.upstream_pools.is_empty());
        let HttpRouteAction::StaticFiles {
            index_files, etag, ..
        } = &config.http_services[0].routes[0].action
        else {
            panic!("static route action");
        };
        let expected = if index.is_empty() {
            vec!["index.html".to_owned()]
        } else {
            vec!["home.html".to_owned()]
        };
        assert_eq!(index_files, &expected);
        assert!(*etag);
    }
}

#[test]
fn lowers_inherited_etag_off_for_actual_alias_try_files_and_headers_shape() {
    let report = import_source(
        r#"http {
          etag off;
          server {
            listen 127.0.0.1:8088 default_server;
            location /assets/ {
              alias /srv/assets;
              try_files $uri =404;
              add_header Cache-Control "public, max-age=3600" always;
            }
            location / { return 404; }
          }
        }"#,
    );

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    let config = report.config.expect("static alias config");
    let HttpRouteAction::StaticFiles {
        path_mapping,
        try_files,
        headers,
        etag,
        ..
    } = &config.http_services[0].routes[0].action
    else {
        panic!("static route action");
    };
    assert_eq!(*path_mapping, oxiroute_config::HttpStaticPathMapping::Alias);
    assert_eq!(
        try_files,
        &[
            oxiroute_config::HttpStaticTryFile::RequestPath,
            oxiroute_config::HttpStaticTryFile::Status { status: 404 },
        ]
    );
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].name, "cache-control");
    assert_eq!(headers[0].value, "public, max-age=3600");
    assert!(headers[0].always);
    assert!(!etag);
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| entry.path == "/http_services/0/routes/0/action/etag")
    );
}

#[test]
fn rejects_invalid_nginx_etag_forms() {
    for directive in ["etag enabled;", "etag 0;", "etag on off;"] {
        let report = import_source(&format!(
            "http {{ {directive} server {{ listen 127.0.0.1:8088 default_server; location / {{ root /srv/www; }} }} }}"
        ));
        assert!(report.has_errors(), "accepted {directive}");
        assert!(report.config.is_none(), "accepted {directive}");
    }
}

#[test]
fn bare_nginx_proxy_timeouts_preserve_seconds_for_slow_upstreams() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_connect_timeout 600;
          proxy_read_timeout 600;
          proxy_send_timeout 600;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_next_upstream off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          server {
            listen 127.0.0.1:8096 default_server;
            location / { proxy_pass http://127.0.0.1:4096; }
          }
        }",
    );

    let config = report.config.expect("bare timeout proxy config");
    let policy = config.http_services[0].routes[0].policy;
    assert_eq!(policy.connect_timeout_ms, 600_000);
    assert_eq!(policy.read_timeout_ms, 600_000);
    assert_eq!(policy.write_timeout_ms, 600_000);
}

#[test]
fn explicit_proxy_headers_cookie_rewrite_and_safe_retry_subset_finalize() {
    let source = r"http {
          proxy_http_version 1.1;
          proxy_connect_timeout 30s;
          proxy_read_timeout 30s;
          proxy_send_timeout 30s;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_next_upstream error timeout;
          proxy_next_upstream_tries 2;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          proxy_pass_header Server;
          upstream backend { server 127.0.0.1:8080; server 127.0.0.1:8081; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name proxy.example;
            location / {
              proxy_pass http://backend;
              proxy_set_header Host upstream.example;
              proxy_set_header X-Client-IP $remote_addr;
              proxy_hide_header X-Powered-By;
              proxy_cookie_path / /application;
            }
          }
        }";
    let broad_retry = import_source(source);
    assert!(broad_retry.config.is_none());
    assert!(broad_retry.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("post-connect request, response-header, and I/O failures")
    }));

    let report = import_source(&source.replace(
        "proxy_next_upstream error timeout;\n          proxy_next_upstream_tries 2;",
        "proxy_next_upstream off;",
    ));

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let route = &report.config.as_ref().expect("proxy config").http_services[0].routes[0];
    let HttpRouteAction::Proxy { policy, .. } = &route.action else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.max_retries, 0);
    assert_eq!(policy.request_headers.len(), 1);
    assert_eq!(policy.response_headers.len(), 8);
    assert_eq!(
        policy
            .response_headers
            .iter()
            .map(|mutation| match mutation {
                oxiroute_config::HttpResponseHeaderMutation::Remove { name } => name.as_str(),
                oxiroute_config::HttpResponseHeaderMutation::Set { .. }
                | oxiroute_config::HttpResponseHeaderMutation::Add { .. } => {
                    panic!("remove policy")
                }
            })
            .collect::<Vec<_>>(),
        [
            "date",
            "x-pad",
            "x-accel-expires",
            "x-accel-redirect",
            "x-accel-limit-rate",
            "x-accel-buffering",
            "x-accel-charset",
            "x-powered-by",
        ]
    );
    assert_eq!(policy.response_cookie_path_rewrites.len(), 1);
}

#[test]
fn blocks_proxy_pass_header_date_that_pingora_replaces() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_next_upstream off;
          proxy_set_header Host $http_host;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          proxy_pass_header Date;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(report.config.is_none());
    assert!(report.draft.listeners.is_empty());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("proxy_pass_header Date")
            && diagnostic.message().contains("Pingora replaces")
    }));
}

#[test]
fn blocks_x_accel_response_controls_that_the_runtime_does_not_implement() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_next_upstream off;
          proxy_set_header Host $http_host;
          proxy_pass_header X-Accel-Redirect;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(report.has_errors());
    assert!(report.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.message().contains("proxy_ignore_headers")
            || diagnostic.message().contains("X-Accel")
    }));
}

#[test]
fn one_named_upstream_is_shared_by_routes_and_listeners() {
    let mut locations = String::from("location / { proxy_pass http://backend; }\n");
    for index in 0..64 {
        writeln!(
            &mut locations,
            "location /route-{index} {{ proxy_pass http://backend; }}"
        )
        .expect("write location");
    }
    let report = import_source(&format!(
        r"http {{
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_next_upstream off;
          proxy_set_header Host $http_host;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend {{ server 127.0.0.1:8080; server 127.0.0.1:8081; }}
          server {{
            listen 127.0.0.1:8088 default_server;
            {locations}
          }}
          server {{
            listen 127.0.0.1:8089 default_server;
            location / {{ proxy_pass http://backend; }}
          }}
        }}"
    ));

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let config = report.config.as_ref().expect("shared upstream config");
    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.http_services.len(), 2);
    assert_eq!(config.http_services[0].routes.len(), 65);
    assert_eq!(config.upstream_pools.len(), 1);
    let pool_name = &config.upstream_pools[0].name;
    assert!(config.http_services.iter().all(|service| {
        service.routes.iter().all(|route| {
            matches!(&route.action, HttpRouteAction::Proxy { upstream_pool, .. } if upstream_pool == pool_name)
        })
    }));

    let upstream = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"upstream")
        .expect("upstream occurrence");
    let upstream_servers = report
        .occurrence_ledger
        .iter()
        .filter(|decision| {
            decision.parent == Some(upstream.occurrence) && decision.name.value == b"server"
        })
        .map(|decision| decision.occurrence)
        .collect::<Vec<_>>();
    assert_eq!(upstream_servers.len(), 2);
    let pool_provenance = report
        .provenance
        .iter()
        .filter(|entry| entry.path.starts_with("/upstream_pools/0"))
        .collect::<Vec<_>>();
    assert_eq!(pool_provenance.len(), 15);
    let origins = |path: &str| {
        report
            .provenance
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("missing provenance {path}"))
            .origins
            .iter()
            .map(|origin| origin.occurrence)
            .collect::<Vec<_>>()
    };
    for suffix in ["", "/name", "/algorithm", "/http_versions"] {
        assert_eq!(
            origins(&format!("/upstream_pools/0{suffix}")),
            [upstream.occurrence]
        );
    }
    assert_eq!(origins("/upstream_pools/0/servers"), upstream_servers);
    for (index, occurrence) in upstream_servers.into_iter().enumerate() {
        for suffix in ["", "/name"] {
            assert_eq!(
                origins(&format!("/upstream_pools/0/servers/{index}{suffix}")),
                [occurrence]
            );
        }
        for suffix in ["", "/type", "/address"] {
            assert_eq!(
                origins(&format!(
                    "/upstream_pools/0/servers/{index}/endpoint{suffix}"
                )),
                [occurrence]
            );
        }
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

    assert!(report.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Resolve
            && diagnostic.message().contains("not an HTTP fragment")
            && diagnostic.message().contains("only an http block")
    }));
}

#[test]
fn lowers_global_gzip_and_log_semantics_but_blocks_mismatched_tls_policy() {
    for directive in ["gzip off;", "access_log off;"] {
        let source = format!(
            "http {{ proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; {directive} upstream backend {{ server 127.0.0.1:8080; }} server {{ listen 127.0.0.1:8080 default_server; server_name test.example; location / {{ proxy_pass http://backend; }} }} }}"
        );
        let report = import_source(&source);
        assert!(report.blocked_services.is_empty());
        assert!(report.config.is_some(), "{:?}", report.diagnostics);
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
            location / { proxy_pass http://backend; }
          }
          server {
            listen 127.0.0.1:8443 ssl;
            server_name two.example;
            ssl_certificate /etc/two-chain.pem;
            ssl_certificate_key /etc/two-key.pem;
            ssl_protocols TLSv1.3;
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
        report.config.as_ref().unwrap().http_services[0].gzip,
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
        report.config.unwrap().http_services[0].gzip,
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
        report.config.unwrap().http_services[0]
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
    assert_eq!(report.config.unwrap().http_services[0].gzip, None);
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
    assert!(report.config.is_some(), "{:?}", report.diagnostics);
    assert_eq!(
        report.config.unwrap().http_services[0]
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
        assert!(report.config.is_none(), "accepted {directive}");
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

    let config = report.config.unwrap();
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

#[test]
fn omitted_access_log_fails_closed_instead_of_disabling_nginx_default_logging() {
    let directory = tempfile::tempdir().expect("create source directory");
    fs::write(
        directory.path().join("nginx.conf"),
        "http { server { listen 127.0.0.1:8080 default_server; location / { return 204; } } }",
    )
    .expect("write source");

    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
    assert!(report.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.message().contains("omitted nginx access_log")
            && diagnostic.message().contains("default combined log")
    }));
}

#[test]
fn lowers_inherited_add_header_for_every_action_and_rejects_dynamic_values() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:9000; }
          server {
            listen 127.0.0.1:8080 default_server;
            add_header X-Inherited inherited;
            location /proxy { add_header Strict-Transport-Security max-age=10 always; proxy_pass http://backend; }
            location = /fixed { return 404; }
            location = /redirect { return 302 /new; }
            location /static { root /srv/www; add_header X-Static static always; }
            location / { return 204; }
          }
        }",
    );
    let config = report.config.expect("literal add_header policy");
    let routes = &config.http_services[0].routes;
    let HttpRouteAction::Proxy { policy, .. } = &routes[0].action else {
        panic!("proxy action");
    };
    assert!(policy.response_headers.iter().any(|header| matches!(
        header,
        oxiroute_config::HttpResponseHeaderMutation::Add { name, always: true, .. }
            if name == "strict-transport-security"
    )));
    assert!(!policy.response_headers.iter().any(|header| {
        matches!(header, oxiroute_config::HttpResponseHeaderMutation::Add { name, .. } if name == "x-inherited")
    }));
    let HttpRouteAction::FixedResponse { headers, .. } = &routes[1].action else {
        panic!("fixed action");
    };
    assert_eq!(headers[0].name, "x-inherited");
    assert!(!headers[0].always);
    let HttpRouteAction::Redirect { headers, .. } = &routes[2].action else {
        panic!("redirect action");
    };
    assert_eq!(headers[0].name, "x-inherited");
    let HttpRouteAction::StaticFiles { headers, mime, .. } = &routes[3].action else {
        panic!("static action");
    };
    assert!(headers[0].always);
    assert_eq!(mime.default_type.as_deref(), Some("text/plain"));

    let report = import_source(
        r"http { server { listen 127.0.0.1:8080 default_server; add_header Strict-Transport-Security $host always; location / { return 204; } } }",
    );
    assert!(report.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("variables in nginx policy values")
    }));
}

#[test]
fn phoenix_shaped_error_page_is_an_explicit_internal_redirect() {
    let report = import_source(
        r"http {
          default_type application/octet-stream;
          server {
            listen 127.0.0.1:8080 default_server;
            location / { root /usr/share/nginx/html; index index.html index.htm; }
            error_page 500 502 503 504 /50x.html;
            location = /50x.html { root /usr/share/nginx/html; }
          }
        }",
    );
    let config = report.config.expect("Phoenix static server");
    let HttpRouteAction::StaticFiles {
        error_responses, ..
    } = &config.http_services[0].routes[0].action
    else {
        panic!("static root action");
    };
    assert_eq!(
        error_responses[0].internal_redirect.as_deref(),
        Some("/50x.html")
    );
}

#[test]
fn blocks_error_page_semantics_on_actions_without_error_rerouting() {
    for source in [
        r"http { server { listen 127.0.0.1:8080 default_server; error_page 404 /404.html; location / { return 404; } location = /404.html { root /srv/www; } } }",
        r"http { proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; upstream app { server 127.0.0.1:9000; } server { listen 127.0.0.1:8080 default_server; error_page 502 /50x.html; location / { proxy_pass http://app; } location = /50x.html { root /srv/www; } } }",
    ] {
        let report = import_source(source);
        assert!(report.config.is_none());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message().contains("error_page") })
        );
    }
}

#[test]
fn blocks_implicit_nginx_proxy_defaults_and_unrepresented_tls_or_logging_policy() {
    for omitted in [
        "proxy_http_version",
        "proxy_buffering",
        "proxy_request_buffering",
        "proxy_ignore_headers",
    ] {
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
        assert!(report.config.is_none(), "omitted {omitted}");
    }

    for directive in [
        "access_log /var/log/nginx/access.log combined;",
        "ssl_ciphers HIGH;",
        "ssl_dhparam /etc/nginx/dh.pem;",
        "ssl_session_cache shared:SSL:1m;",
        "ssl_session_tickets off;",
        "ssl_session_timeout 5m;",
    ] {
        let source = format!(
            "http {{ proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; upstream backend {{ server 127.0.0.1:8080; }} server {{ listen 127.0.0.1:8443 ssl default_server; server_name test.example; ssl_certificate /etc/test.pem; ssl_certificate_key /etc/test.key; ssl_protocols TLSv1.2 TLSv1.3; {directive} location / {{ proxy_pass http://backend; }} }} }}"
        );
        let report = import_source(&source);
        assert!(report.has_errors(), "accepted {directive}");
        assert!(report.config.is_none(), "accepted {directive}");
    }
}

#[test]
fn lowers_nginx_host_separately_from_http_host_with_server_name_fallback() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name fallback.example;
            location / {
              proxy_set_header Host $host;
              proxy_set_header X-Original-Authority $http_host;
              proxy_set_header X-Nginx-Host $host;
              proxy_pass http://backend;
            }
          }
        }",
    );
    let config = report.config.as_ref().expect("nginx host policy");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route");
    };
    assert_eq!(
        policy.upstream_host,
        HttpUpstreamHost::NginxHost {
            fallback: "fallback.example".into()
        }
    );
    assert!(policy.request_headers.iter().any(|mutation| matches!(
        mutation,
        oxiroute_config::HttpRequestHeaderMutation::Set {
            value: oxiroute_config::HttpRequestHeaderValue::NginxHost { fallback },
            ..
        } if fallback == "fallback.example"
    )));
}

#[test]
fn nginx_host_fallback_serializes_ipv6_as_an_http_authority() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          server {
            listen [::1]:8088 default_server;
            server_name 2001:db8::1;
            location / {
              proxy_set_header Host $host;
              proxy_pass http://[::1]:8080;
            }
          }
        }",
    );

    let config = report.config.expect("IPv6 nginx host fallback");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route");
    };
    assert_eq!(
        policy.upstream_host,
        HttpUpstreamHost::NginxHost {
            fallback: "[2001:db8::1]".into()
        }
    );
}

#[test]
fn blocks_unmodeled_try_files_reroutes_but_models_exact_index_policy_reselection() {
    let report = import_source(
        r"http { server { listen 127.0.0.1:8080 default_server; server_name static.example; location / { root /srv/www; try_files $uri /private/index.html; } location /private { root /srv/www; auth_basic private; auth_basic_user_file /etc/nginx/users; } } }",
    );
    assert!(report.has_errors(), "unsafe try_files reroute was lowered");
    assert!(report.config.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("internal rerouting") })
    );

    let report = import_source(
        r"http { server { listen 127.0.0.1:8080 default_server; server_name static.example; location / { root /srv/www; index private.html; } location = /private.html { root /srv/www; auth_basic private; auth_basic_user_file /etc/nginx/users; } } }",
    );
    let config = report.config.expect("exact index reroute is represented");
    let HttpRouteAction::StaticFiles {
        internal_index_redirects,
        ..
    } = &config.http_services[0].routes[0].action
    else {
        panic!("static route");
    };
    assert!(*internal_index_redirects);
}

fn fixture(name: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let source = fs::read(Path::new("tests/fixtures/nginx").join(name)).expect("read fixture");
    let certificate =
        fs::canonicalize("tests/fixtures/nginx/proxy.pem").expect("canonical certificate fixture");
    let private_key = copy_secure_key(&directory, "proxy-key.pem", "proxy-key.pem");
    let source = String::from_utf8_lossy(&source)
        .replacen("http {", "http { access_log off;", 1)
        .replace("@CERTIFICATE@", &certificate.to_string_lossy())
        .replace("@PRIVATE_KEY@", &private_key.to_string_lossy());
    fs::write(directory.path().join("nginx.conf"), source).expect("write fixture");
    directory
}

fn copy_secure_key(directory: &TempDir, fixture: &str, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = directory.path().join(name);
    fs::copy(Path::new("tests/fixtures/nginx").join(fixture), &path).expect("copy test key");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure test key mode");
    path
}

fn import_source(source: &str) -> oxiroute_import::nginx::ImportReport {
    let directory = tempfile::tempdir().expect("create source directory");
    let source = source.replacen("http {", "http { access_log off;", 1);
    fs::write(directory.path().join("nginx.conf"), source).expect("write source");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}
