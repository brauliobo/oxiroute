use std::{fmt::Write as _, path::Path};

use oxiroute_config::HttpRouteAction;
use oxiroute_import::{DiagnosticStage, nginx::import_http_fragment};

use super::support::{fixture, import_source};

#[test]
fn bounded_upstream_weights_lower_with_default_one_and_provenance() {
    let report = import_source(
        r"http {
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_next_upstream off;
          proxy_next_upstream_tries 1;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend {
            server 127.0.0.1:8080 weight=50;
            server 127.0.0.1:8081;
          }
          server {
            listen 127.0.0.1:8088 default_server;
            location / { proxy_pass http://backend; }
          }
        }",
    );

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("weighted nginx config");
    assert_eq!(
        config.upstream_pools[0].algorithm,
        oxiroute_config::UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![50, 1]
        }
    );
    for path in [
        "/upstream_pools/0/algorithm/weights/0",
        "/upstream_pools/0/algorithm/weights/1",
    ] {
        assert!(
            report.provenance.iter().any(|entry| entry.path == path),
            "{path}"
        );
    }
}

#[test]
fn dns_upstream_shared_by_tcp_and_unix_listeners_finalizes_without_a_placeholder() {
    let directory = fixture("synthetic/nginx/dns-upstream-two-listeners.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    assert_eq!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .listeners
            .len(),
        2
    );
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic.stage() == DiagnosticStage::Lower
            && diagnostic.message().contains("static IP endpoint")
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("shared upstream config");
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
