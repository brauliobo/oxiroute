use oxiroute_config::{HttpPathSelector, HttpRouteAction};

use super::support::import_source;

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
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .http_services[0]
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
fn lowers_static_index_behavior_into_canonical_static_routes() {
    for index in ["", "index home.html;"] {
        let report = import_source(&format!(
            "http {{ server {{ listen 127.0.0.1:8088 default_server; location / {{ root /srv/www; {index} }} }} }}"
        ));

        let config = report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .expect("static config");
        assert!(report.blocked_services.is_empty());
        assert_eq!(report.summary().upstream_pools, 0);
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("static alias config");
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
fn caret_tilde_is_a_plain_prefix_only_without_regex_named_or_variable_locations() {
    let server = |locations: &str| {
        format!(
            "http {{ server {{ listen 127.0.0.1:8080 default_server; server_name test.example; {locations} location / {{ return 204; }} }} }}"
        )
    };
    let report = import_source(&server("location ^~ /assets/ { return 200 ok; }"));
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("^~ lowers as a prefix");
    assert!(config.http_services[0].routes.iter().any(|route| {
        route.path
            == HttpPathSelector::RawPrefix {
                value: "/assets/".into(),
            }
    }));

    for special in [
        "location ~ ^/x { return 204; }",
        "location @named { return 204; }",
        "location /$v { return 204; }",
    ] {
        let report = import_source(&server(&format!(
            "location ^~ /assets/ {{ return 200 ok; }} {special}"
        )));
        assert!(report.validated().is_none(), "{special}");
        assert!(
            report.diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message()
                    .contains("only a plain prefix in a server without regex")
            }),
            "{special}"
        );
    }

    let duplicate = import_source(&server(
        "location ^~ /assets/ { return 200 ok; } location /assets/ { return 200 ok; }",
    ));
    assert!(duplicate.has_errors());
}

#[test]
fn bearer_authorization_gate_is_limited_to_a_location_and_a_401_return() {
    for (label, source) in [
        (
            "server level",
            "http { upstream app { server 127.0.0.1:9000; } server { listen 127.0.0.1:8080 default_server; if ($http_authorization != \"Bearer test-token-0000\") { return 401; } location / { proxy_pass http://app; } } }",
        ),
        (
            "other status",
            "http { upstream app { server 127.0.0.1:9000; } server { listen 127.0.0.1:8080 default_server; location / { if ($http_authorization != \"Bearer test-token-0000\") { return 403; } proxy_pass http://app; } } }",
        ),
        (
            "not bearer",
            "http { upstream app { server 127.0.0.1:9000; } server { listen 127.0.0.1:8080 default_server; location / { if ($http_authorization != \"Basic dGVzdA==\") { return 401; } proxy_pass http://app; } } }",
        ),
    ] {
        let report = import_source(source);
        assert!(report.has_errors(), "{label}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message().contains("bearer-authorization subset")),
            "{label}"
        );
    }
}

#[test]
fn blocks_unmodeled_try_files_reroutes_but_models_exact_index_policy_reselection() {
    let report = import_source(
        r"http { server { listen 127.0.0.1:8080 default_server; server_name static.example; location / { root /srv/www; try_files $uri /private/index.html; } location /private { root /srv/www; auth_basic private; auth_basic_user_file /etc/nginx/users; } } }",
    );
    assert!(report.has_errors(), "unsafe try_files reroute was lowered");
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
            .any(|diagnostic| { diagnostic.message().contains("internal rerouting") })
    );

    let report = import_source(
        r"http { server { listen 127.0.0.1:8080 default_server; server_name static.example; location / { root /srv/www; index private.html; } location = /private.html { root /srv/www; auth_basic private; auth_basic_user_file /etc/nginx/users; } } }",
    );
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("exact index reroute is represented");
    let HttpRouteAction::StaticFiles {
        internal_index_redirects,
        ..
    } = &config.http_services[0].routes[0].action
    else {
        panic!("static route");
    };
    assert!(*internal_index_redirects);
}
