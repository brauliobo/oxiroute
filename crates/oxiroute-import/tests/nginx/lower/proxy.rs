use std::path::Path;

use oxiroute_config::{
    HttpHostSelector, HttpPathSelector, HttpRouteAction, HttpSameSite, HttpUpstreamHost,
    UpstreamConnectionReuse,
};
use oxiroute_import::{DiagnosticStage, nginx::import_http_fragment};

use super::support::{fixture, import_source};

#[test]
fn fully_explicit_proxy_fixture_finalizes_with_canonical_routes() {
    let directory = fixture("nginx/representable.conf");
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized nginx config");
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
    let independently_validated = config.clone().validate().expect("canonical validation");
    assert_eq!(independently_validated.as_draft(), config);
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
fn lowers_proxy_pass_uri_replacement_and_accepts_explicit_expires_off() {
    let report = import_source(
        r"http {
          expires off;
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_next_upstream off;
          proxy_next_upstream_tries 1;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            location /api/ { proxy_pass http://backend/v1/; }
            location / { return 404; }
          }
        }",
    );

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("rewritten proxy config");
    let route = config.http_services[0]
        .routes
        .iter()
        .find(|route| {
            route.path
                == HttpPathSelector::RawPrefix {
                    value: "/api/".into(),
                }
        })
        .expect("rewritten proxy route");
    let HttpRouteAction::Proxy { policy, .. } = &route.action else {
        panic!("rewritten proxy action");
    };
    let rewrite = policy
        .upstream_path_rewrite
        .as_ref()
        .expect("canonical upstream path rewrite");
    assert_eq!(rewrite.from, "/api/");
    assert_eq!(rewrite.to, "/v1/");
    assert!(report.provenance.iter().any(|entry| {
        entry
            .path
            .ends_with("/action/policy/upstream_path_rewrite/to")
    }));
    assert!(
        serde_json::to_string(config)
            .expect("serialized rewritten proxy config")
            .contains("upstream_path_rewrite")
    );
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
    assert_eq!(report.summary().listeners, 1);
    assert_eq!(report.summary().http_services, 1);
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("canonical proxy defaults");
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

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("bare timeout proxy config");
    let policy = config.http_services[0].routes[0].policy;
    assert_eq!(policy.connect_timeout_ms, 600_000);
    assert_eq!(policy.read_timeout_ms, 600_000);
    assert_eq!(policy.write_timeout_ms, 600_000);
    assert_eq!(config.http_services[0].upstream_io_timeout_ms, 600_000);
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
    assert!(
        broad_retry
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
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
    let route = &report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("proxy config")
        .http_services[0]
        .routes[0];
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
fn lowers_exact_proxy_cookie_flags_and_records_attribute_provenance() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          proxy_cookie_flags session secure nohttponly samesite=lax;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    let route = &report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .unwrap()
        .http_services[0]
        .routes[0];
    let HttpRouteAction::Proxy { policy, .. } = &route.action else {
        panic!("proxy route");
    };
    assert_eq!(
        policy.response_cookie_attributes,
        vec![oxiroute_config::HttpCookieAttributePolicy {
            name: "session".into(),
            secure: Some(true),
            http_only: Some(false),
            same_site: Some(HttpSameSite::Lax),
        }]
    );
    let source_origin = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"proxy_cookie_flags")
        .expect("cookie flag decision")
        .span;
    let provenance = report
        .provenance
        .iter()
        .find(|provenance| {
            provenance.path
                == "/http_services/0/routes/0/action/policy/response_cookie_attributes/0/name"
        })
        .expect("cookie attribute provenance");
    assert!(
        provenance
            .origins
            .iter()
            .any(|origin| origin.span == source_origin)
    );

    for directive in [
        "proxy_cookie_flags ~session secure;",
        "proxy_cookie_flags $cookie_name secure;",
        "proxy_cookie_flags session nosamesite;",
        "proxy_cookie_flags session unknown;",
        "proxy_cookie_flags session secure secure;",
        "proxy_cookie_flags off;",
    ] {
        let report = import_source(&format!(
            "http {{ proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; {directive} upstream backend {{ server 127.0.0.1:8080; }} server {{ listen 127.0.0.1:8088 default_server; location / {{ proxy_pass http://backend; }} }} }}"
        ));
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

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(report.summary().listeners, 0);
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
    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.message().contains("proxy_ignore_headers")
            || diagnostic.message().contains("X-Accel")
    }));
}

#[test]
fn omitted_proxy_buffering_lowers_to_streaming_with_a_warning_per_location() {
    let report = import_source(
        "http { proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; upstream backend { server 127.0.0.1:8080; } server { listen 127.0.0.1:8080 default_server; server_name test.example; location /a { proxy_pass http://backend; } location / { proxy_pass http://backend; } } }",
    );
    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("default proxy_buffering finalizes");
    assert!(
        config.http_services[0]
            .routes
            .iter()
            .all(|route| !route.policy.response_buffering)
    );
    let warnings = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code().as_str() == "W_PROXY_BUFFERING_STREAMED")
        .count();
    assert_eq!(warnings, 2);
}

#[test]
fn regex_proxy_cookie_path_is_blocked_instead_of_ignored() {
    let report = import_source(
        "http { proxy_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; upstream app { server 127.0.0.1:9000; } server { listen 127.0.0.1:8080 default_server; location / { proxy_pass http://app; proxy_cookie_path ~*^/a(.*) /b$1; } } }",
    );
    assert!(report.has_errors());
}
