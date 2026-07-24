#![cfg(unix)]

use std::{fmt::Write as _, fs, path::Path};

use oxiroute_config::{
    HttpHostSelector, HttpPathSelector, HttpRouteAction, HttpUpstreamHost, ListenerBind,
    validate_config,
};
use oxiroute_import::{
    DiagnosticStage, E_SEMANTICS_NOT_REPRESENTABLE, nginx::import_http_fragment,
};
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
    assert_eq!(config.http_services[0].routes.len(), 3);
    assert!(matches!(
        config.http_services[0].routes[1].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/api"
    ));
    assert!(matches!(
        config.http_services[0].routes[2].host,
        Some(HttpHostSelector::NormalizedHost { ref value }) if value == "other.example.test"
    ));
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
        "/upstream_pools/0/endpoints/0/address",
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
fn blocks_every_non_root_nginx_raw_prefix() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name default.example;
            location / { proxy_pass http://backend; }
            location /api { proxy_pass http://backend; }
          }
        }",
    );

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("proxy_buffering")
    }));
}

#[test]
fn default_servers_also_require_a_representable_root_catch_all() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
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
fn blocks_unrepresented_proxy_defaults_and_listener_admission() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name proxy.example;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    for message in [
        "proxy_set_header Host must be explicit",
        "proxy_buffering must be explicitly disabled",
        "proxy_next_upstream must be explicit",
    ] {
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message().contains(message))
        );
    }
    assert!(report.draft.listeners.is_empty());
    assert!(report.draft.http_services.is_empty());
}

#[test]
fn blocks_non_default_servers_without_a_representable_local_catch_all() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
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
fn blocks_nginx_leading_wildcards_instead_of_widening_host_matching() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
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

    assert_eq!(report.blocked_services.len(), 1);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("leading wildcard")
    }));
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
fn audits_one_certificate_lineage_across_distinct_tls_binds_without_finalizing() {
    let directory = tempfile::tempdir().expect("create TLS source directory");
    let certificate =
        fs::canonicalize("tests/fixtures/nginx/proxy.pem").expect("canonical certificate fixture");
    let private_key = copy_secure_key(&directory, "proxy-key.pem", "proxy-key.pem");
    let source = format!(
        r"http {{
          proxy_http_version 1.1;
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
    assert_eq!(report.blocked_services.len(), 2);
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("private key material") })
    );
}

#[test]
fn blocks_unreadable_or_unsupported_certificate_metadata_without_details() {
    for (label, certificate) in [
        ("missing", "/definitely/missing/nginx-certificate.pem"),
        ("malformed", "/tmp/oxiroute-nginx-malformed-certificate.pem"),
    ] {
        let directory = tempfile::tempdir().expect("create TLS source directory");
        let certificate_path = if label == "malformed" {
            let path = directory.path().join("malformed.pem");
            fs::write(&path, b"not a PEM certificate").expect("write malformed certificate");
            path.to_string_lossy().into_owned()
        } else {
            certificate.to_owned()
        };
        let private_key = copy_secure_key(&directory, "proxy-key.pem", "proxy-key.pem");
        let source = tls_source(&certificate_path, &private_key.to_string_lossy());
        fs::write(directory.path().join("nginx.conf"), source).expect("write TLS source");

        let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
        assert_eq!(report.blocked_services.len(), 1, "{label}");
        assert!(
            report.diagnostics.iter().any(|diagnostic| {
                diagnostic.stage() == DiagnosticStage::Lower
                    && diagnostic.message().contains("certificate metadata")
            }),
            "{label}: {:?}",
            report.diagnostics
        );
    }
}

#[test]
fn blocks_unsafe_or_mismatched_private_keys_without_exposing_details() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    for case in [
        "missing",
        "malformed",
        "mismatch",
        "symlink",
        "directory",
        "insecure",
    ] {
        let directory = tempfile::tempdir().expect("create TLS key source directory");
        let certificate = fs::canonicalize("tests/fixtures/nginx/proxy.pem")
            .expect("canonical certificate fixture");
        let key = directory.path().join("candidate-key.pem");
        match case {
            "missing" => {}
            "malformed" => {
                fs::write(&key, b"not a private key").expect("write malformed key");
                fs::set_permissions(&key, fs::Permissions::from_mode(0o600))
                    .expect("secure malformed key mode");
            }
            "mismatch" => {
                copy_secure_key(&directory, "proxy-mismatched-key.pem", "candidate-key.pem");
            }
            "symlink" => {
                let target = copy_secure_key(&directory, "proxy-key.pem", "target-key.pem");
                symlink(target, &key).expect("create key symlink");
            }
            "directory" => fs::create_dir(&key).expect("create key directory"),
            "insecure" => {
                fs::copy("tests/fixtures/nginx/proxy-key.pem", &key).expect("copy insecure key");
                fs::set_permissions(&key, fs::Permissions::from_mode(0o644))
                    .expect("set insecure key mode");
            }
            _ => unreachable!("bounded key case"),
        }
        let source = tls_source(&certificate.to_string_lossy(), &key.to_string_lossy());
        fs::write(directory.path().join("nginx.conf"), source).expect("write TLS key source");

        let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
        let key_diagnostics = report
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message().contains("private key material"))
            .collect::<Vec<_>>();
        assert_eq!(key_diagnostics.len(), 1, "{case}: {:?}", report.diagnostics);
        assert_eq!(
            key_diagnostics[0].message(),
            "private key material is unreadable or unsupported"
        );
        assert!(
            !key_diagnostics[0]
                .message()
                .contains(key.to_string_lossy().as_ref())
        );
    }
}

#[test]
fn keeps_explicit_ipv6_proxy_topology_draft_only() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
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

    assert_eq!(report.blocked_services.len(), 2);
}

#[test]
fn hostrouter_shaped_dns_service_stays_blocked_without_a_placeholder() {
    let directory = fixture("hostrouter-partial.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert_eq!(report.blocked_services.len(), 2);
    let dns_block = report
        .blocked_services
        .iter()
        .find(|blocked| blocked.path == "/nginx/http/0/binds/1")
        .expect("DNS upstream bind blocker");
    assert_eq!(dns_block.servers.len(), 1);
    assert!(
        dns_block
            .diagnostic_codes
            .contains(&E_SEMANTICS_NOT_REPRESENTABLE)
    );
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
          upstream backend { server backend.internal:8080; }
          server {
            listen unix:/run/nginx/proxy.sock default_server;
            server_name proxy.example;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(matches!(
        report.blocked_services[0].bind,
        Some(ListenerBind::Unix { ref path }) if path == Path::new("/run/nginx/proxy.sock")
    ));
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("not an explicit socket or canonical Unix address")
    }));
}

#[test]
fn blocks_unsupported_nginx_behavior_without_emitting_partial_services() {
    let cases = [
        (
            "DNS upstream",
            "upstream backend { server backend.lan:8080; }",
            "location / { proxy_pass http://backend; }",
        ),
        (
            "Unix upstream",
            "upstream backend { server unix:/run/backend.sock; }",
            "location / { proxy_pass http://backend; }",
        ),
        (
            "variable origin",
            "",
            "location / { proxy_pass http://$backend; }",
        ),
        (
            "insecure HTTPS",
            "upstream backend { server 127.0.0.1:8443; }",
            "location / { proxy_pass https://backend; }",
        ),
        (
            "header policy",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; proxy_set_header Host $host; }",
        ),
        (
            "authentication",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; auth_request /auth; }",
        ),
        (
            "cookie rewriting",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; proxy_cookie_path / /secure; }",
        ),
        (
            "buffering",
            "upstream backend { server 127.0.0.1:8080; }",
            "location / { proxy_pass http://backend; proxy_buffering off; }",
        ),
        (
            "ambiguous path",
            "upstream backend { server 127.0.0.1:8080; }",
            "location /api/ { proxy_pass http://backend; }",
        ),
    ];

    for (label, upstream, location) in cases {
        let source = format!(
            "http {{ proxy_http_version 1.1; {upstream} server {{ listen 127.0.0.1:8080 default_server; server_name test.example; {location} }} }}"
        );
        let report = import_source(&source);
        assert_eq!(report.blocked_services.len(), 1, "{label}");
        assert!(report.draft.listeners.is_empty(), "{label}");
        assert!(report.draft.http_services.is_empty(), "{label}");
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
    assert!(matches!(
        config.http_services[0].routes[1].action,
        HttpRouteAction::Redirect { status: 308, .. }
    ));
    assert!(matches!(
        config.http_services[0].routes[2].action,
        HttpRouteAction::FixedResponse { status: 404, .. }
    ));
    assert!(config.upstream_pools.is_empty());
    assert!(report.provenance.iter().all(|provenance| {
        !provenance.path.contains("/action/upstream_pool")
            && !provenance.path.contains("/action/policy")
    }));
}

#[test]
fn blocks_static_index_behavior_that_would_skip_nginx_location_reselection() {
    for index in ["", "index home.html;"] {
        let report = import_source(&format!(
            "http {{ server {{ listen 127.0.0.1:8088 default_server; location / {{ root /srv/www; {index} }} }} }}"
        ));

        assert!(report.config.is_none());
        assert_eq!(report.blocked_services.len(), 1);
        assert!(report.draft.listeners.is_empty());
        assert!(report.draft.http_services.is_empty());
        assert!(report.draft.upstream_pools.is_empty());
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.stage() == DiagnosticStage::Lower
                && diagnostic.message().contains("internally redirects")
                && diagnostic.message().contains("location selection")
        }));
    }
}

#[test]
fn explicit_proxy_headers_cookie_rewrite_and_safe_retry_subset_finalize() {
    let report = import_source(
        r"http {
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
        }",
    );

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let route = &report.config.as_ref().expect("proxy config").http_services[0].routes[0];
    let HttpRouteAction::Proxy { policy, .. } = &route.action else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.max_retries, 1);
    assert_eq!(policy.retry.triggers.len(), 2);
    assert_eq!(policy.request_headers.len(), 1);
    assert_eq!(policy.response_headers.len(), 8);
    assert_eq!(
        policy
            .response_headers
            .iter()
            .map(|mutation| match mutation {
                oxiroute_config::HttpResponseHeaderMutation::Remove { name } => name.as_str(),
                oxiroute_config::HttpResponseHeaderMutation::Set { .. } => panic!("remove policy"),
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
fn blocks_unrepresented_x_accel_response_controls() {
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

    assert!(report.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("X-Accel response controls")
            && diagnostic.message().contains("proxy_ignore_headers")
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
    assert_eq!(pool_provenance.len(), 11);
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
    assert_eq!(origins("/upstream_pools/0/endpoints"), upstream_servers);
    for (index, occurrence) in upstream_servers.into_iter().enumerate() {
        for suffix in ["", "/type", "/address"] {
            assert_eq!(
                origins(&format!("/upstream_pools/0/endpoints/{index}{suffix}")),
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
fn blocks_global_gzip_and_log_semantics_and_mismatched_tls_policy() {
    for directive in ["gzip off;", "access_log off;"] {
        let source = format!(
            "http {{ proxy_http_version 1.1; {directive} upstream backend {{ server 127.0.0.1:8080; }} server {{ listen 127.0.0.1:8080 default_server; server_name test.example; location / {{ proxy_pass http://backend; }} }} }}"
        );
        let report = import_source(&source);
        assert_eq!(report.blocked_services.len(), 1);
    }

    let report = import_source(
        r"http {
          proxy_http_version 1.1;
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

fn fixture(name: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let source = fs::read(Path::new("tests/fixtures/nginx").join(name)).expect("read fixture");
    let certificate =
        fs::canonicalize("tests/fixtures/nginx/proxy.pem").expect("canonical certificate fixture");
    let private_key = copy_secure_key(&directory, "proxy-key.pem", "proxy-key.pem");
    let source = String::from_utf8_lossy(&source)
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

fn tls_source(certificate: &str, private_key: &str) -> String {
    format!(
        r"http {{
          proxy_http_version 1.1;
          upstream backend {{ server 127.0.0.1:8080; }}
          server {{
            listen 127.0.0.1:8443 ssl default_server;
            server_name invented.example.test;
            ssl_certificate {certificate};
            ssl_certificate_key {private_key};
            ssl_protocols TLSv1.2 TLSv1.3;
            location / {{ proxy_pass http://backend; }}
          }}
        }}"
    )
}

fn import_source(source: &str) -> oxiroute_import::nginx::ImportReport {
    let directory = tempfile::tempdir().expect("create source directory");
    fs::write(directory.path().join("nginx.conf"), source).expect("write source");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}
