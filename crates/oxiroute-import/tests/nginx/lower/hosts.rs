use oxiroute_config::{HttpHostSelector, HttpRouteAction, HttpUpstreamHost};
use oxiroute_import::DiagnosticStage;

use super::support::import_source;

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

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("default server host selectors");
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
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .http_services[0]
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
    let hosts = mixed
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .unwrap()
        .http_services[0]
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
    let hosts = leading_dot_first
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .unwrap()
        .http_services[0]
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("nginx host policy");
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
fn certbot_server_if_precedes_only_its_exact_host_return_fallback() {
    let report = import_source(
        r"http {
          server {
            listen 127.0.0.1:8080 default_server;
            if ($host = llama.corp-a.test) {
              return 301 https://$host$request_uri;
            }
            server_name llama.corp-a.test;
            return 404;
          }
        }",
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("Certbot server return routes");
    let routes = &config.http_services[0].routes;
    assert_eq!(routes.len(), 2);
    assert!(matches!(
        (&routes[0].host, &routes[0].action),
        (
            Some(HttpHostSelector::NormalizedHost { value }),
            HttpRouteAction::Redirect { status: 301, .. }
        ) if value == "llama.corp-a.test"
    ));
    assert!(matches!(
        (&routes[1].host, &routes[1].action),
        (None, HttpRouteAction::FixedResponse { status: 404, .. })
    ));
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

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("IPv6 nginx host fallback");
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
