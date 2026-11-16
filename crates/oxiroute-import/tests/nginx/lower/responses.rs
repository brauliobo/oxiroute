use oxiroute_config::{HttpPathSelector, HttpRouteAction};

use super::support::import_source;

#[test]
fn imported_http_services_enable_automatic_response_headers_in_canonical_rendering() {
    let report = import_source(
        "http { server { listen 127.0.0.1:8088 default_server; location / { return 200 ok; } } }",
    );
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("nginx HTTP config");

    assert!(
        config
            .http_services
            .iter()
            .all(|service| service.automatic_response_headers)
    );
    let source = serde_json::to_string(config).expect("serialized nginx import");
    assert!(source.contains("\"automatic_response_headers\":true"));
    assert!(!source.contains("\"automatic_response_headers\":false"));
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("fixed/static nginx config");
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("literal add_header policy");
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
    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("variables in nginx policy values")
    }));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one response fixture covers lowering, rendering, and provenance"
)]
fn lowers_literal_response_directives_with_order_statuses_provenance_and_rendering() {
    let source = r"http {
      add_header X-Http http;
      server {
        listen 127.0.0.1:8080 default_server;
        add_header X-Server server;
        location = /fixed { return 404; }
        location = /redirect {
          add_header X-Duplicate first;
          add_header X-Duplicate second always;
          return 302 /new;
        }
        location /static {
          root /srv/www;
          add_header X-Static static;
          error_page 404 /404.html;
        }
        location = /404.html {
          root /srv/www;
          add_header X-Error error always;
        }
        location / { return 204; }
      }
    }";
    let report = import_source(source);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("literal response directives");

    let fixed = config.http_services[0]
        .routes
        .iter()
        .find(|route| {
            matches!(
                route.action,
                HttpRouteAction::FixedResponse { status: 404, .. }
            )
        })
        .expect("fixed route");
    let HttpRouteAction::FixedResponse { headers, .. } = &fixed.action else {
        unreachable!();
    };
    assert_eq!(headers[0].name, "x-server");
    assert!(!headers[0].always);

    let redirect = config.http_services[0]
        .routes
        .iter()
        .find(|route| matches!(route.action, HttpRouteAction::Redirect { status: 302, .. }))
        .expect("redirect route");
    let HttpRouteAction::Redirect { headers, .. } = &redirect.action else {
        unreachable!();
    };
    assert_eq!(
        headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str(), header.always))
            .collect::<Vec<_>>(),
        [
            ("x-duplicate", "first", false),
            ("x-duplicate", "second", true),
        ]
    );

    let static_route = config.http_services[0]
        .routes
        .iter()
        .find(|route| matches!(route.action, HttpRouteAction::StaticFiles { .. }))
        .expect("static route");
    let HttpRouteAction::StaticFiles {
        headers,
        error_responses,
        ..
    } = &static_route.action
    else {
        unreachable!();
    };
    assert_eq!(headers[0].name, "x-static");
    assert_eq!(error_responses[0].statuses, [404]);
    assert_eq!(
        error_responses[0].internal_redirect.as_deref(),
        Some("/404.html")
    );

    let rendered = serde_json::to_string(config).expect("serialized literal response directives");
    assert!(rendered.contains("\"name\":\"x-static\""));
    assert!(rendered.contains("x-duplicate"));
    assert!(rendered.contains("error_responses"));
    assert!(rendered.contains("\"internal_redirect\":\"/404.html\""));

    let header_provenance = report
        .provenance
        .iter()
        .find(|entry| entry.path.ends_with("/action/headers/0"))
        .expect("header provenance");
    assert!(header_provenance.origins.iter().any(|origin| {
        report
            .source_graph
            .source(origin.span.source())
            .and_then(|source| source.source.slice(origin.span.range()))
            .is_some_and(|bytes| bytes.starts_with(b"add_header"))
    }));
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| entry.path.ends_with("/action/headers/0/always"))
    );
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| entry.path.ends_with("/action/error_responses/0/statuses/0"))
    );
    let error_provenance = report
        .provenance
        .iter()
        .find(|entry| entry.path.ends_with("/action/error_responses/0"))
        .expect("error-page provenance");
    assert!(error_provenance.origins.iter().any(|origin| {
        report
            .source_graph
            .source(origin.span.source())
            .and_then(|source| source.source.slice(origin.span.range()))
            .is_some_and(|bytes| bytes.starts_with(b"error_page"))
    }));
}

#[test]
fn rejects_invalid_duplicate_and_unsupported_response_directive_forms_with_source_diagnostics() {
    for (directive, message) in [
        (
            "add_header bad\\ name value;",
            "response header name is invalid",
        ),
        (
            "add_header X-Header \"bad\\nvalue\";",
            "response header value contains invalid bytes",
        ),
        (
            "error_page 404 404 /404.html;",
            "error_page statuses must be unique",
        ),
        ("error_page 404 =200 /404.html;", "400..=599 status list"),
        (
            "error_page 404 https://example.test/404;",
            "absolute canonical URI target",
        ),
        ("expires 1h;", "unsupported"),
    ] {
        let source = format!(
            "http {{ access_log off; server {{ listen 127.0.0.1:8080 default_server; {directive} location / {{ root /srv/www; }} location = /404.html {{ root /srv/www; }} }} }}"
        );
        let report = import_source(&source);
        assert!(
            report
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "accepted {directive}"
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message().contains(message)
                    && diagnostic.primary_span().is_some()),
            "missing source diagnostic for {directive}: {:#?}",
            report.diagnostics
        );
    }
}

#[test]
fn static_server_error_page_is_an_explicit_internal_redirect() {
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("static server");
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
        assert!(
            report
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none()
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message().contains("error_page") })
        );
    }
}
