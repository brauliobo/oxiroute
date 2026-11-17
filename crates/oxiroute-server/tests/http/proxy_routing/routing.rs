use crate::TEST_TIMEOUT;
use crate::http_support::HttpResponse as RawResponse;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::Origin;
use crate::support::wire::assert_origin_response;
use crate::support::wire::read_request_head;
use oxiroute_config::HttpRequestHeaderMutation;
use oxiroute_config::HttpRequestHeaderValue;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::UpstreamAlgorithm;
use oxiroute_import::nginx::NginxDefaultErrorPageOverlay;
use oxiroute_import::nginx::NginxImportOptions;
use oxiroute_import::nginx::import_root_with_options;
use std::fs;
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[tokio::test]
async fn imported_nginx_overlay_preserves_certbot_and_proxy_error_responses_on_wire() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("origin", 1).await;
        let directory = tempfile::tempdir().expect("nginx source directory");
        fs::write(
            directory.path().join("nginx.conf"),
            format!(
                "events {{}} http {{ access_log off; proxy_buffering off; upstream app {{ server {}; }} server {{ listen 127.0.0.1:8080 default_server; if ($host = llama.corp-a.test) {{ return 301 https://$host$request_uri; }} server_name llama.corp-a.test; return 404; }} server {{ listen 127.0.0.1:8080; server_name proxy.test; location / {{ proxy_pass http://app; }} }} }}",
                origin.address
            ),
        )
        .expect("write nginx source");
        let report = import_root_with_options(
            Path::new("nginx.conf"),
            directory.path(),
            &NginxImportOptions {
                default_error_page: Some(NginxDefaultErrorPageOverlay {
                    server: "nginx/1.30.4".into(),
                }),
                x_accel_controls_absent: true,
                ..NginxImportOptions::default()
            },
        );
        assert!(!report.has_errors(), "{:#?}", report.diagnostics);
        assert!(report.candidate.operational_overlays.iter().any(|overlay| {
            overlay.kind == oxiroute_import::OperationalOverlayKind::DefaultErrorPageMigration
                && overlay.satisfied
        }));
        let mut config = report.candidate.validated().expect("imported nginx config").to_draft();
        let service = config.http_services.remove(0);
        let proxy = ProxyHarness::start(
            config.upstream_pools,
            service.routes,
            service.max_request_body_bytes.unwrap_or(1024),
            7,
        )
        .await;

        let redirect_body = nginx_error_body_bytes(301, "Moved Permanently", "nginx/1.30.4");
        assert_eq!(redirect_body.len(), 169);
        let redirect = proxy
            .request("GET /path?query=1 HTTP/1.1\r\nHost: llama.corp-a.test\r\n")
            .await;
        assert_eq!(redirect.status, 301);
        assert_eq!(redirect.header("server"), Some("nginx/1.30.4"));
        assert_eq!(redirect.header("content-type"), Some("text/html"));
        assert_eq!(redirect.header("content-length"), Some("169"));
        assert_eq!(
            redirect.header("location"),
            Some("https://llama.corp-a.test/path?query=1")
        );
        assert_eq!(redirect.body(), redirect_body);
        let redirect_head = proxy
            .request("HEAD /path HTTP/1.1\r\nHost: llama.corp-a.test\r\n")
            .await;
        assert_eq!(redirect_head.status, 301);
        assert_eq!(redirect_head.header("content-length"), Some("169"));
        assert!(redirect_head.body().is_empty());

        let not_found_body = nginx_error_body_bytes(404, "Not Found", "nginx/1.30.4");
        assert_eq!(not_found_body.len(), 153);
        let not_found = proxy
            .request("GET /unknown HTTP/1.1\r\nHost: unknown.test\r\n")
            .await;
        assert_eq!(not_found.status, 404);
        assert_eq!(not_found.header("server"), Some("nginx/1.30.4"));
        assert_eq!(not_found.header("content-type"), Some("text/html"));
        assert_eq!(not_found.header("content-length"), Some("153"));
        assert_eq!(not_found.body(), not_found_body);
        let not_found_head = proxy
            .request("HEAD /unknown HTTP/1.1\r\nHost: unknown.test\r\n")
            .await;
        assert_eq!(not_found_head.status, 404);
        assert_eq!(not_found_head.header("content-length"), Some("153"));
        assert!(not_found_head.body().is_empty());

        let success = proxy
            .request("GET / HTTP/1.1\r\nHost: proxy.test\r\n")
            .await;
        assert_eq!(success.status, 200);
        assert_eq!(success.header("server"), Some("nginx/1.30.4"));
        assert_eq!(success.header("content-type"), None);
        assert_eq!(success.body(), b"origin");
        origin.finish().await;

        let bad_gateway_body = nginx_error_body_bytes(502, "Bad Gateway", "nginx/1.30.4");
        assert_eq!(bad_gateway_body.len(), 157);
        let bad_gateway = proxy
            .request("GET / HTTP/1.1\r\nHost: proxy.test\r\n")
            .await;
        assert_eq!(bad_gateway.status, 502);
        assert_eq!(bad_gateway.header("server"), Some("nginx/1.30.4"));
        assert_eq!(bad_gateway.header("content-type"), Some("text/html"));
        assert_eq!(bad_gateway.header("content-length"), Some("157"));
        assert_eq!(bad_gateway.body(), bad_gateway_body);
        let bad_gateway_head = proxy
            .request("HEAD / HTTP/1.1\r\nHost: proxy.test\r\n")
            .await;
        assert_eq!(bad_gateway_head.status, 502);
        assert_eq!(bad_gateway_head.header("content-length"), Some("157"));
        assert!(bad_gateway_head.body().is_empty());

        proxy.finish().await;
    })
    .await
    .expect("imported nginx response test timed out");
}

#[tokio::test]
async fn selects_routes_by_host_path_and_method_over_real_http_connections() {
    timeout(TEST_TIMEOUT, async {
        let exact = Origin::start("exact", 1).await;
        let writer = Origin::start("writer", 1).await;
        let fallback = Origin::start("fallback", 3).await;
        let proxy = ProxyHarness::start(
            vec![
                pool("exact", &[exact.address]),
                pool("writer", &[writer.address]),
                pool("fallback", &[fallback.address]),
            ],
            vec![
                route(Some("api.example.test"), "/v1/items", &["GET"], "exact"),
                route(Some("api.example.test"), "/v1/items", &["POST"], "writer"),
                route(None, "/", &[], "fallback"),
            ],
            1024,
            5,
        )
        .await;

        let exact_response = proxy
            .request("GET /v1/items/42 HTTP/1.1\r\nHost: API.Example.TEST:8443\r\n")
            .await;
        assert_origin_response(&exact_response, "exact");

        let writer_response = proxy
            .request(
                "POST /v1/items/42 HTTP/1.1\r\nHost: api.example.test\r\nContent-Length: 0\r\n",
            )
            .await;
        assert_origin_response(&writer_response, "writer");

        let wrong_method = proxy
            .request("DELETE /v1/items/42 HTTP/1.1\r\nHost: api.example.test\r\n")
            .await;
        assert_origin_response(&wrong_method, "fallback");

        let wrong_path = proxy
            .request("GET /v1/other HTTP/1.1\r\nHost: api.example.test\r\n")
            .await;
        assert_origin_response(&wrong_path, "fallback");

        let wrong_host = proxy
            .request("GET /v1/items/42 HTTP/1.1\r\nHost: other.example.test\r\n")
            .await;
        assert_origin_response(&wrong_host, "fallback");

        proxy.finish().await;
        exact.finish().await;
        writer.finish().await;
        fallback.finish().await;
    })
    .await
    .expect("route selection test timed out");
}

#[tokio::test]
async fn nginx_suffix_routes_use_importer_resolved_first_wins_order_on_wire() {
    timeout(TEST_TIMEOUT, async {
        let fallback = Origin::start("fallback", 1).await;
        let wildcard = Origin::start("wildcard", 1).await;
        let longest = Origin::start("longest", 1).await;
        let leading_dot = Origin::start("leading-dot", 2).await;
        let mut wildcard_route = route(None, "/", &[], "wildcard");
        wildcard_route.host = Some(oxiroute_config::HttpHostSelector::NginxLeadingWildcard {
            value: "example.test".into(),
        });
        let mut longest_route = route(None, "/", &[], "longest");
        longest_route.host = Some(oxiroute_config::HttpHostSelector::NginxLeadingWildcard {
            value: "deep.example.test".into(),
        });
        let mut leading_dot_route = route(None, "/", &[], "leading-dot");
        leading_dot_route.host = Some(oxiroute_config::HttpHostSelector::NginxLeadingDot {
            value: "base.test".into(),
        });
        let proxy = ProxyHarness::start(
            vec![
                pool("fallback", &[fallback.address]),
                pool("wildcard", &[wildcard.address]),
                pool("longest", &[longest.address]),
                pool("leading-dot", &[leading_dot.address]),
            ],
            vec![
                route(None, "/", &[], "fallback"),
                wildcard_route,
                longest_route,
                leading_dot_route,
            ],
            1024,
            5,
        )
        .await;

        assert_origin_response(
            &proxy
                .request("GET / HTTP/1.1\r\nHost: example.test\r\n")
                .await,
            "fallback",
        );
        assert_origin_response(
            &proxy
                .request("GET / HTTP/1.1\r\nHost: a.b.example.test\r\n")
                .await,
            "wildcard",
        );
        assert_origin_response(
            &proxy
                .request("GET / HTTP/1.1\r\nHost: edge.deep.example.test\r\n")
                .await,
            "longest",
        );
        for host in ["base.test", "a.b.base.test"] {
            assert_origin_response(
                &proxy
                    .request(&format!("GET / HTTP/1.1\r\nHost: {host}\r\n"))
                    .await,
                "leading-dot",
            );
        }

        proxy.finish().await;
        fallback.finish().await;
        wildcard.finish().await;
        longest.finish().await;
        leading_dot.finish().await;
    })
    .await
    .expect("suffix route test timed out");
}

#[tokio::test]
async fn round_robins_two_origins_across_independent_requests() {
    timeout(TEST_TIMEOUT, async {
        let first = Origin::start("first", 1).await;
        let second = Origin::start("second", 1).await;
        let proxy = ProxyHarness::start(
            vec![pool("balanced", &[first.address, second.address])],
            vec![route(None, "/", &[], "balanced")],
            1024,
            2,
        )
        .await;

        let first_response = proxy
            .request("GET / HTTP/1.1\r\nHost: round-robin.test\r\n")
            .await;
        let second_response = proxy
            .request("GET / HTTP/1.1\r\nHost: round-robin.test\r\n")
            .await;

        assert_origin_response(&first_response, "first");
        assert_origin_response(&second_response, "second");

        proxy.finish().await;
        first.finish().await;
        second.finish().await;
    })
    .await
    .expect("round-robin test timed out");
}

#[tokio::test]
async fn least_connections_sends_a_concurrent_request_to_the_idle_origin() {
    timeout(TEST_TIMEOUT, async {
        let held_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("held origin bind");
        let held_address = held_listener.local_addr().expect("held origin address");
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let held_task = tokio::spawn(async move {
            let (mut stream, _) = held_listener.accept().await.expect("held origin accept");
            read_request_head(&mut stream)
                .await
                .expect("held origin request");
            accepted_tx.send(()).expect("held origin accepted signal");
            release_rx.await.expect("held origin release");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nheld")
                .await
                .expect("held origin response");
        });
        let idle = Origin::start("idle", 1).await;
        let mut balanced = pool("balanced", &[held_address, idle.address]);
        balanced.algorithm = UpstreamAlgorithm::LeastConnections;
        balanced.connection_reuse = oxiroute_config::UpstreamConnectionReuse::Never;
        let proxy = ProxyHarness::start(
            vec![balanced],
            vec![route(None, "/", &[], "balanced")],
            1024,
            2,
        )
        .await;

        let mut held_client = TcpStream::connect(proxy.address)
            .await
            .expect("held client connect");
        held_client
            .write_all(b"GET / HTTP/1.1\r\nHost: least.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("held client request");
        accepted_rx
            .await
            .expect("first request reached held origin");
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0].active_connections,
            1
        );

        let idle_response = proxy
            .request("GET / HTTP/1.1\r\nHost: least.test\r\n")
            .await;
        assert_origin_response(&idle_response, "idle");

        release_tx.send(()).expect("release held origin");
        let mut held_response = Vec::new();
        held_client
            .read_to_end(&mut held_response)
            .await
            .expect("held client response");
        assert_origin_response(&RawResponse::parse(held_response), "held");
        held_task.await.expect("held origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
        idle.finish().await;
    })
    .await
    .expect("least-connections test timed out");
}

#[tokio::test]
async fn returns_404_without_contacting_an_origin_when_no_route_matches() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("unmatched", 1).await;
        let proxy = ProxyHarness::start(
            vec![pool("api", &[origin.address])],
            vec![route(Some("api.example.test"), "/v1", &["GET"], "api")],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request("GET /v2 HTTP/1.1\r\nHost: api.example.test\r\n")
            .await;

        assert_eq!(response.status, 404, "response: {}", response.text());
        assert!(response.text().contains("route not found"));
        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("no-route test timed out");
}

#[tokio::test]
async fn rejects_ambiguous_authorities_without_contacting_an_origin() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("authority", 1).await;
        let proxy = ProxyHarness::start(
            vec![pool("default", &[origin.address])],
            vec![route(None, "/", &[], "default")],
            1024,
            3,
        )
        .await;

        let duplicate = proxy
            .request("GET / HTTP/1.1\r\nHost: first.test\r\nHost: first.test\r\n")
            .await;
        assert_eq!(duplicate.status, 400, "response: {}", duplicate.text());

        let conflicting = proxy
            .request("GET http://first.test/ HTTP/1.1\r\nHost: second.test\r\n")
            .await;
        assert_eq!(conflicting.status, 400, "response: {}", conflicting.text());

        let userinfo = proxy
            .request("GET http://user@first.test/ HTTP/1.1\r\nHost: user@first.test\r\n")
            .await;
        assert_eq!(userinfo.status, 400, "response: {}", userinfo.text());

        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("authority validation test timed out");
}

#[tokio::test]
async fn rejects_paths_with_ambiguous_upstream_normalization() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("ambiguous-path", 1).await;
        let proxy = ProxyHarness::start(
            vec![pool("default", &[origin.address])],
            vec![route(None, "/", &[], "default")],
            1024,
            5,
        )
        .await;

        for path in [
            "/public/../admin",
            "/public/%2e%2e/admin",
            "/public%2fadmin",
            "/%70rivate",
            "//private",
        ] {
            let response = proxy
                .request(&format!("GET {path} HTTP/1.1\r\nHost: path.test\r\n"))
                .await;
            assert_eq!(response.status, 400, "response: {}", response.text());
        }

        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("ambiguous path test timed out");
}

#[tokio::test]
async fn rejects_bounded_request_header_expansion_before_contacting_the_origin() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("unused", 1).await;
        let mut proxy_route = route(None, "/", &[], "origin");
        let HttpRouteAction::Proxy { policy, .. } = &mut proxy_route.action else {
            unreachable!();
        };
        policy.request_headers = vec![HttpRequestHeaderMutation::Set {
            name: "x-request-id".into(),
            value: HttpRequestHeaderValue::IncomingHeader {
                name: "x-request-id".into(),
                max_bytes: 4,
            },
        }];
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin.address])],
            vec![proxy_route],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: bounded.test\r\nX-Request-Id: five5\r\n")
            .await;
        assert_eq!(response.status, 431, "response: {}", response.text());

        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("bounded request header test timed out");
}

fn nginx_error_body_bytes(status: u16, reason: &str, server: &str) -> Vec<u8> {
    format!(
        "<html>\r\n<head><title>{status} {reason}</title></head>\r\n<body>\r\n<center><h1>{status} {reason}</h1></center>\r\n<hr><center>{server}</center>\r\n</body>\r\n</html>\r\n"
    )
    .into_bytes()
}
