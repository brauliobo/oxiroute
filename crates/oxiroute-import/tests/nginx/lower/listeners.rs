use std::path::Path;

use oxiroute_config::ListenerBind;

use super::support::import_source;

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
    assert_eq!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .listeners
            .len(),
        2
    );
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
        report.validated().map(oxiroute_config::ValidatedConfig::as_draft).unwrap().listeners[0].bind,
        ListenerBind::Unix { ref path, .. } if path == Path::new("/run/nginx/proxy.sock")
    ));
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("not an explicit socket or canonical Unix address")
    }));
}

#[test]
fn lowers_effective_nginx_keepalive_timeout_into_listener_downstream_policy() {
    let report = import_source(
        r"http {
          keepalive_timeout 65s;
          server {
            listen 127.0.0.1:8088 default_server;
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
            .listeners[0]
            .downstream_timeouts
            .keepalive_timeout_ms,
        Some(65_000)
    );
    assert!(report.provenance.iter().any(|provenance| {
        provenance.path == "/listeners/0/downstream_timeouts/keepalive_timeout_ms"
    }));

    for directive in ["keepalive_timeout 0;", "keepalive_timeout 65s 60s;"] {
        let report = import_source(&format!(
            "http {{ {directive} server {{ listen 127.0.0.1:8088 default_server; location / {{ return 204; }} }} }}"
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
