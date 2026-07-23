#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;

use std::{
    fs, io,
    net::SocketAddr,
    os::unix::fs::symlink,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use oxiroute_config::{
    Config, HealthCheck, HealthCheckType, HttpAccessPolicy, HttpCookiePathRewrite,
    HttpLiteralHeader, HttpPathSelector, HttpProxyPolicy, HttpRedirectLocation,
    HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation,
    HttpRetryTrigger, HttpRoute, HttpRouteAction, HttpService, HttpUpstreamHost, HttpVersionPolicy,
    Listener, Protocol, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool,
};
use oxiroute_server::{
    HttpReverseProxy, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RoundRobinPool, RuntimeMetrics,
    ServiceKind, runtime_plan,
};
use pingora::{apps::ServerApp, proxy::http_proxy, server::configuration::ServerConf};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use fixture_support::{create_secure_root, write_secure_token};
use http_support::{
    HttpResponse as RawResponse, raw_http_request, read_request_head as read_request_head_bytes,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const NO_ORIGIN_CONTACT_WINDOW: Duration = Duration::from_millis(100);

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
            proxy.pools[0].health_snapshot().endpoints[0].active_leases,
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
async fn connects_to_a_dns_http_endpoint_and_keeps_its_identity() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("dns", 1).await;
        let proxy = ProxyHarness::start(
            vec![endpoint_pool(
                "dns",
                vec![UpstreamEndpoint::Dns {
                    host: "localhost".into(),
                    port: origin.address.port(),
                }],
                UpstreamAlgorithm::RoundRobin,
            )],
            vec![route(None, "/", &[], "dns")],
            1024,
            1,
        )
        .await;

        let response = proxy.request("GET / HTTP/1.1\r\nHost: dns.test\r\n").await;
        assert_origin_response(&response, "dns");
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0]
                .address
                .to_string(),
            format!("localhost:{}", origin.address.port())
        );

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("DNS HTTP test timed out");
}

#[cfg(unix)]
#[tokio::test]
async fn connects_to_a_unix_http_endpoint() {
    use tokio::net::UnixListener;

    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("Unix HTTP directory");
        let path = directory.path().join("origin.sock");
        let listener = UnixListener::bind(&path).expect("Unix HTTP bind");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("Unix HTTP accept");
            read_request_head_from(&mut stream)
                .await
                .expect("Unix HTTP request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nunix")
                .await
                .expect("Unix HTTP response");
        });
        let proxy = ProxyHarness::start(
            vec![endpoint_pool(
                "unix",
                vec![UpstreamEndpoint::Unix { path: path.clone() }],
                UpstreamAlgorithm::RoundRobin,
            )],
            vec![route(None, "/", &[], "unix")],
            1024,
            1,
        )
        .await;

        let response = proxy.request("GET / HTTP/1.1\r\nHost: unix.test\r\n").await;
        assert_origin_response(&response, "unix");

        proxy.finish().await;
        origin.await.expect("Unix HTTP origin task");
    })
    .await
    .expect("Unix HTTP test timed out");
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
async fn returns_503_when_a_matched_pool_has_no_healthy_endpoint() {
    timeout(TEST_TIMEOUT, async {
        let mut origin = Origin::start("unhealthy", 1).await;
        let mut checked_pool = pool("api", &[origin.address]);
        checked_pool.health_check = Some(HealthCheck {
            kind: HealthCheckType::Tcp,
            interval_ms: 1_000,
            timeout_ms: 100,
            healthy_threshold: 1,
            unhealthy_threshold: 1,
            host: None,
            path: None,
        });
        let origin_task = origin.task.take().expect("stop origin before probe");
        origin_task.abort();
        let _ = origin_task.await;
        let proxy = ProxyHarness::start_with_health(
            vec![checked_pool],
            vec![route(None, "/", &[], "api")],
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: unavailable.test\r\n")
            .await;

        assert_eq!(response.status, 503, "response: {}", response.text());
        assert_eq!(proxy.pools[0].health_snapshot().unavailable_selections, 1);
        proxy.finish().await;
    })
    .await
    .expect("unhealthy pool test timed out");
}

#[tokio::test]
async fn rejects_oversized_content_length_before_contacting_an_origin() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("oversized", 1).await;
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[origin.address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            8,
            1,
        )
        .await;

        let response = proxy
            .request("POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 9\r\n")
            .await;

        assert_eq!(response.status, 413, "response: {}", response.text());
        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("oversized Content-Length test timed out");
}

#[tokio::test]
async fn rejects_a_chunked_body_when_streaming_crosses_the_limit() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_silent().await;
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[origin.address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            8,
            1,
        )
        .await;

        let response = proxy
            .request_bytes(
                b"POST /upload HTTP/1.1\r\n\
                  Host: upload.test\r\n\
                  Transfer-Encoding: chunked\r\n\
                  Connection: close\r\n\
                  \r\n\
                  4\r\n1234\r\n\
                  5\r\n56789\r\n\
                  0\r\n\r\n",
            )
            .await;

        assert_eq!(response.status, 413, "response: {}", response.text());
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("chunked overflow test timed out");
}

#[tokio::test]
async fn an_unbounded_body_limit_streams_without_size_rejection() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("unbounded", 1).await;
        let proxy = ProxyHarness::start_unbounded(
            vec![pool("upload", &[origin.address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            1,
        )
        .await;
        let body = vec![b'x'; 32 * 1024];
        let mut request = format!(
            "POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);

        let response = proxy.request_bytes(&request).await;
        assert_origin_response(&response, "unbounded");

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("unbounded body test timed out");
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
async fn enforces_the_connection_cap_in_the_production_http_wrapper() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("limited", 1).await;
        let proxy = ProxyHarness::start_with_limit(
            vec![pool("default", &[origin.address])],
            vec![route(None, "/", &[], "default")],
            1024,
            1,
            2,
        )
        .await;

        let mut first = TcpStream::connect(proxy.address)
            .await
            .expect("first client connect");
        first
            .write_all(b"GET / HTTP/1.1\r\nHost: limited.test\r\n")
            .await
            .expect("partial first request");
        proxy.wait_for_active_connections(1).await;

        let mut second = TcpStream::connect(proxy.address)
            .await
            .expect("second client connect");
        let mut byte = [0; 1];
        let closed = timeout(Duration::from_secs(1), second.read(&mut byte))
            .await
            .expect("limited connection must close promptly");
        assert!(
            matches!(closed, Ok(0) | Err(_)),
            "limited connection stayed open"
        );

        let snapshot = proxy.metrics.snapshot().expect("connection-cap snapshot");
        assert_eq!(snapshot.traffic.accepted_connections, 2);
        assert_eq!(snapshot.traffic.rejected_connections, 1);
        assert_eq!(snapshot.traffic.active_connections, 1);

        drop(first);
        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("connection-cap test timed out");
}

#[tokio::test]
async fn retries_a_bodyless_get_on_a_distinct_endpoint_after_connect_failure() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let healthy = Origin::start("retry-success", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, healthy.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_origin_response(&response, "retry-success");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("connect retry test timed out");
}

#[tokio::test]
async fn retries_a_bodyless_head_after_connect_failure() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let healthy = Origin::start("head-success", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, healthy.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("HEAD / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_eq!(response.status, 200, "response: {}", response.text());
        proxy.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("HEAD retry test timed out");
}

#[tokio::test]
async fn an_omitted_retry_budget_fails_after_the_first_endpoint() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let unused = Origin::start("must-not-receive", 1).await;
        let proxy = ProxyHarness::start(
            vec![pool("retry", &[unavailable, unused.address])],
            vec![route(None, "/", &[], "retry")],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_eq!(response.status, 502, "response: {}", response.text());
        proxy.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("default retry budget test timed out");
}

#[tokio::test]
async fn does_not_retry_an_unsafe_method_after_connect_failure() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let unused = Origin::start("must-not-receive", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, unused.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("POST / HTTP/1.1\r\nHost: retry.test\r\nContent-Length: 0\r\n")
            .await;

        assert_eq!(response.status, 502, "response: {}", response.text());
        proxy.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("unsafe retry test timed out");
}

#[tokio::test]
async fn does_not_retry_a_get_with_a_request_body() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let unused = Origin::start("must-not-receive", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, unused.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request_bytes(
                b"GET / HTTP/1.1\r\nHost: retry.test\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody",
            )
            .await;

        assert_eq!(response.status, 502, "response: {}", response.text());
        proxy.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("body retry test timed out");
}

#[tokio::test]
async fn does_not_retry_after_an_upstream_connection_is_established() {
    timeout(TEST_TIMEOUT, async {
        let disconnecting = Origin::start_disconnecting().await;
        let unused = Origin::start("must-not-receive", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[disconnecting.address, unused.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_eq!(response.status, 502, "response: {}", response.text());
        proxy.finish().await;
        disconnecting.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("post-connect retry test timed out");
}

#[tokio::test]
async fn does_not_retry_an_upgrade_request() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let unused = Origin::start("must-not-receive", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, unused.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request_bytes(
                b"GET / HTTP/1.1\r\nHost: retry.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
            )
            .await;

        assert_eq!(response.status, 502, "response: {}", response.text());
        proxy.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("upgrade retry test timed out");
}

#[tokio::test]
async fn does_not_retry_an_upstream_error_status() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = Origin::start_status(503, "unavailable").await;
        let unused = Origin::start("must-not-receive", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable.address, unused.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_eq!(response.status, 503, "response: {}", response.text());
        proxy.finish().await;
        unavailable.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("status retry test timed out");
}

#[tokio::test]
async fn stops_when_the_configured_retry_budget_is_exhausted() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_addresses(2).await;
        let unused = Origin::start("over-budget", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool(
                "retry",
                &[unavailable[0], unavailable[1], unused.address],
            )],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_eq!(response.status, 502, "response: {}", response.text());
        proxy.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("retry budget test timed out");
}

#[tokio::test]
async fn permits_two_retries_and_succeeds_on_the_third_endpoint() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_addresses(2).await;
        let healthy = Origin::start("third-attempt", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool(
                "retry",
                &[unavailable[0], unavailable[1], healthy.address],
            )],
            vec![route(None, "/", &[], "retry")],
            2,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_origin_response(&response, "third-attempt");
        proxy.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("two-retry test timed out");
}

#[tokio::test]
async fn serves_fixed_and_redirect_actions_with_exact_get_and_head_semantics() {
    timeout(TEST_TIMEOUT, async {
        let proxy = ProxyHarness::start(
            Vec::new(),
            vec![
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/fixed".into(),
                    },
                    methods: vec!["GET".into(), "HEAD".into()],
                    access_policy: None,
                    action: HttpRouteAction::FixedResponse {
                        status: 200,
                        body: "fixed body".into(),
                        headers: vec![HttpLiteralHeader {
                            name: "x-fixed".into(),
                            value: "yes".into(),
                        }],
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/empty".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    action: HttpRouteAction::FixedResponse {
                        status: 204,
                        body: String::new(),
                        headers: Vec::new(),
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/redirect".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    action: HttpRouteAction::Redirect {
                        status: 308,
                        location: HttpRedirectLocation::RequestTemplate {
                            value: "$scheme://$host$request_uri".into(),
                        },
                    },
                },
            ],
            1024,
            4,
        )
        .await;

        let get = proxy
            .request("GET /fixed HTTP/1.1\r\nHost: Actions.Example:8080\r\n")
            .await;
        assert_eq!(get.status, 200);
        assert_eq!(get.header("x-fixed"), Some("yes"));
        assert_eq!(get.header("content-length"), Some("10"));
        assert_eq!(get.body(), b"fixed body");

        let head = proxy
            .request("HEAD /fixed HTTP/1.1\r\nHost: actions.example:8080\r\n")
            .await;
        assert_eq!(head.status, 200);
        assert_eq!(head.header("content-length"), Some("10"));
        assert!(head.body().is_empty());

        let empty = proxy
            .request("GET /empty HTTP/1.1\r\nHost: actions.example\r\n")
            .await;
        assert_eq!(empty.status, 204);
        assert_eq!(empty.header("content-length"), None);
        assert!(empty.body().is_empty());

        let redirect = proxy
            .request("GET /redirect?next=1 HTTP/1.1\r\nHost: Actions.Example:8080\r\n")
            .await;
        assert_eq!(redirect.status, 308);
        assert_eq!(
            redirect.header("location"),
            Some("http://actions.example/redirect?next=1")
        );

        proxy.finish().await;
    })
    .await
    .expect("local action test timed out");
}

#[tokio::test]
async fn bearer_access_uses_the_configured_header_without_exposing_the_token() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("access token directory");
        let token = "route-token-0123456789abcdef0123456789abcdef";
        let token_path = write_secure_token(directory.path(), "route-token", token);
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: Some(HttpAccessPolicy::BearerTokenFile {
                token_file_path: token_path.clone(),
                header_name: "x-route-token".into(),
                realm: Some("private".into()),
            }),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "authorized".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 3).await;

        let missing = proxy
            .request("GET / HTTP/1.1\r\nHost: access.test\r\n")
            .await;
        assert_eq!(missing.status, 401);
        assert_eq!(
            missing.header("www-authenticate"),
            Some("Bearer realm=\"private\"")
        );
        let wrong = proxy
            .request("GET / HTTP/1.1\r\nHost: access.test\r\nX-Route-Token: Bearer wrong\r\n")
            .await;
        assert_eq!(wrong.status, 401);
        let accepted = proxy
            .request(&format!(
                "GET / HTTP/1.1\r\nHost: access.test\r\nX-Route-Token: Bearer {token}\r\n"
            ))
            .await;
        assert_eq!(accepted.status, 200);
        assert_eq!(accepted.body(), b"authorized");
        assert!(!accepted.text().contains(token));
        assert!(!accepted.text().contains(&token_path.display().to_string()));

        proxy.finish().await;
    })
    .await
    .expect("route access test timed out");
}

#[tokio::test]
async fn static_files_pin_the_root_and_reject_symlinks_while_supporting_indexes_and_spa() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("static fixture directory");
        let root = create_secure_root(directory.path(), "public");
        fs::write(root.join("index.html"), b"index").expect("write index");
        fs::write(root.join("app.js"), b"javascript").expect("write asset");
        fs::write(root.join("spa.html"), b"fallback").expect("write fallback");
        symlink("/etc/passwd", root.join("escape.txt")).expect("write escape symlink");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            root.join("special"),
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .expect("write special-file fixture");
        let oversized = fs::File::create(root.join("oversized.bin")).expect("oversized fixture");
        oversized
            .set_len(16 * 1024 * 1024 + 1)
            .expect("oversized fixture length");
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            action: HttpRouteAction::StaticFiles {
                root_directory: root.clone(),
                index_files: vec!["index.html".into()],
                spa_fallback: Some("spa.html".into()),
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 8).await;

        let moved = directory.path().join("pinned-public");
        fs::rename(&root, &moved).expect("move pinned root");
        fs::create_dir(&root).expect("replace configured root path");
        fs::write(root.join("index.html"), b"replacement").expect("write replacement root");

        let index = proxy
            .request("GET / HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(index.status, 200);
        assert_eq!(index.body(), b"index");
        assert_eq!(
            index.header("content-type"),
            Some("text/html; charset=utf-8")
        );

        let head = proxy
            .request("HEAD /app.js HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(head.status, 200);
        assert_eq!(head.header("content-length"), Some("10"));
        assert_eq!(
            head.header("content-type"),
            Some("text/javascript; charset=utf-8")
        );
        assert!(head.body().is_empty());

        let fallback = proxy
            .request("GET /client/route HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(fallback.status, 200);
        assert_eq!(fallback.body(), b"fallback");

        let symlink_response = proxy
            .request("GET /escape.txt HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(symlink_response.status, 403);
        assert!(!symlink_response.text().contains("root:"));

        let special = proxy
            .request("GET /special HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(special.status, 403);

        let oversized = proxy
            .request("GET /oversized.bin HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(oversized.status, 500);

        let method = proxy
            .request("POST /app.js HTTP/1.1\r\nHost: static.test\r\nContent-Length: 0\r\n")
            .await;
        assert_eq!(method.status, 405);
        assert_eq!(method.header("allow"), Some("GET, HEAD"));

        let traversal = proxy
            .request("GET /../etc/passwd HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(traversal.status, 400);

        proxy.finish().await;
    })
    .await
    .expect("static action test timed out");
}

#[tokio::test]
async fn applies_host_header_cookie_and_request_response_header_policies() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("policy origin bind");
        let origin_address = listener.local_addr().expect("policy origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("policy origin accept");
            let request = read_request_head_bytes(&mut stream)
                .await
                .expect("policy origin request");
            let request = String::from_utf8(request).expect("policy request UTF-8");
            assert!(request.contains("\r\nHost: selected.example:9443\r\n"));
            assert!(request.contains("\r\nx-incoming: Client.Example:8080\r\n"));
            assert!(request.contains("\r\nx-normalized: client.example\r\n"));
            assert!(request.contains("\r\nx-selected: selected.example:9443\r\n"));
            assert!(!request.contains("x-remove:"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Remove: old\r\nSet-Cookie: sid=1; Path=/internal; HttpOnly\r\nConnection: close\r\n\r\nok",
                )
                .await
                .expect("policy origin response");
        });
        let mut policy = HttpProxyPolicy {
            upstream_host: HttpUpstreamHost::Literal {
                value: "selected.example:9443".into(),
            },
            request_headers: vec![
                HttpRequestHeaderMutation::Set {
                    name: "x-incoming".into(),
                    value: HttpRequestHeaderValue::IncomingAuthority,
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-normalized".into(),
                    value: HttpRequestHeaderValue::NormalizedHost,
                },
                HttpRequestHeaderMutation::Set {
                    name: "x-selected".into(),
                    value: HttpRequestHeaderValue::SelectedUpstreamHost,
                },
                HttpRequestHeaderMutation::Remove {
                    name: "x-remove".into(),
                },
            ],
            response_headers: vec![
                HttpResponseHeaderMutation::Set {
                    name: "x-added".into(),
                    value: "new".into(),
                },
                HttpResponseHeaderMutation::Remove {
                    name: "x-remove".into(),
                },
            ],
            response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
                from: "/internal".into(),
                to: "/".into(),
            }],
            ..HttpProxyPolicy::default()
        };
        policy.retry.max_retries = 0;
        let mut proxy_route = route(None, "/", &[], "origin");
        let HttpRouteAction::Proxy {
            policy: route_policy,
            ..
        } = &mut proxy_route.action
        else {
            unreachable!();
        };
        *route_policy = policy;
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin_address])],
            vec![proxy_route],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request(
                "GET / HTTP/1.1\r\nHost: Client.Example:8080\r\nX-Remove: client\r\n",
            )
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.header("x-added"), Some("new"));
        assert_eq!(response.header("x-remove"), None);
        assert_eq!(
            response.header("set-cookie"),
            Some("sid=1; Path=/; HttpOnly")
        );

        proxy.finish().await;
        origin.await.expect("policy origin task");
    })
    .await
    .expect("proxy policy test timed out");
}

#[tokio::test]
async fn retries_only_for_a_configured_trigger() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let unused = Origin::start("must-not-receive", 1).await;
        let mut proxy_route = route(None, "/", &[], "retry");
        let HttpRouteAction::Proxy { policy, .. } = &mut proxy_route.action else {
            unreachable!();
        };
        policy.retry.triggers = vec![HttpRetryTrigger::ConnectTimeout];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, unused.address])],
            vec![proxy_route],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;
        assert_eq!(response.status, 502, "response: {}", response.text());

        proxy.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("retry trigger test timed out");
}

#[tokio::test]
async fn preserve_and_endpoint_host_policies_set_the_expected_upstream_authority() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Host-policy origin bind");
        let origin_address = listener.local_addr().expect("Host-policy origin address");
        let origin = tokio::spawn(async move {
            for expected in [
                "host: incoming.example:8080".to_owned(),
                format!("host: {origin_address}"),
            ] {
                let (mut stream, _) = listener.accept().await.expect("Host-policy origin accept");
                let request = read_request_head_bytes(&mut stream)
                    .await
                    .expect("Host-policy request");
                let request = String::from_utf8(request)
                    .expect("Host-policy request UTF-8")
                    .to_ascii_lowercase();
                assert!(request.contains(&expected), "request: {request}");
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .expect("Host-policy response");
            }
        });
        let preserve = route(None, "/preserve", &[], "origin");
        let mut endpoint = route(None, "/endpoint", &[], "origin");
        let HttpRouteAction::Proxy { policy, .. } = &mut endpoint.action else {
            unreachable!();
        };
        policy.upstream_host = HttpUpstreamHost::Endpoint {
            unix_fallback: None,
        };
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin_address])],
            vec![preserve, endpoint],
            1024,
            2,
        )
        .await;

        for path in ["/preserve", "/endpoint"] {
            let response = proxy
                .request(&format!(
                    "GET {path} HTTP/1.1\r\nHost: Incoming.Example:8080\r\n"
                ))
                .await;
            assert_eq!(response.status, 200, "response: {}", response.text());
        }

        proxy.finish().await;
        origin.await.expect("Host-policy origin task");
    })
    .await
    .expect("Host-policy test timed out");
}

struct ProxyHarness {
    address: SocketAddr,
    metrics: RuntimeMetrics,
    pools: Vec<Arc<RoundRobinPool>>,
    _shutdown_tx: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ProxyHarness {
    async fn start(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        max_request_body_bytes: u64,
        expected_connections: usize,
    ) -> Self {
        Self::start_with_policy(
            upstream_pools,
            routes,
            Some(max_request_body_bytes),
            100,
            0,
            false,
            expected_connections,
        )
        .await
    }

    async fn start_with_limit(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        max_request_body_bytes: u64,
        max_connections: u64,
        expected_connections: usize,
    ) -> Self {
        Self::start_with_policy(
            upstream_pools,
            routes,
            Some(max_request_body_bytes),
            max_connections,
            0,
            false,
            expected_connections,
        )
        .await
    }

    async fn start_unbounded(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        expected_connections: usize,
    ) -> Self {
        Self::start_with_policy(
            upstream_pools,
            routes,
            None,
            100,
            0,
            false,
            expected_connections,
        )
        .await
    }

    async fn start_with_retries(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        max_retries: u8,
        expected_connections: usize,
    ) -> Self {
        Self::start_with_policy(
            upstream_pools,
            routes,
            Some(1024),
            100,
            max_retries,
            false,
            expected_connections,
        )
        .await
    }

    async fn start_with_health(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        expected_connections: usize,
    ) -> Self {
        Self::start_with_policy(
            upstream_pools,
            routes,
            Some(1024),
            100,
            0,
            true,
            expected_connections,
        )
        .await
    }

    async fn start_with_policy(
        upstream_pools: Vec<UpstreamPool>,
        mut routes: Vec<HttpRoute>,
        max_request_body_bytes: Option<u64>,
        max_connections: u64,
        max_retries: u8,
        run_health_checks: bool,
        expected_connections: usize,
    ) -> Self {
        for route in &mut routes {
            if let HttpRouteAction::Proxy { policy, .. } = &mut route.action {
                policy.retry.max_retries = max_retries;
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let address = listener.local_addr().expect("proxy address");
        let config = Config {
            listeners: vec![Listener {
                name: "http-loopback".into(),
                bind: socket_bind(address),
                protocol: Protocol::Http,
                service: Some("routing".into()),
                tls_profile: None,
                max_connections: Some(max_connections),
            }],
            upstream_pools,
            http_services: vec![HttpService {
                name: "routing".into(),
                routes,
                upstream_io_timeout_ms: 1_000,
                max_request_body_bytes,
            }],
            ..empty_config()
        };
        let plan = runtime_plan(&config).expect("canonical HTTP service plan");
        let pools = plan.pools.clone();
        if run_health_checks {
            plan.health_supervisor
                .as_ref()
                .expect("health supervisor")
                .probe_once()
                .await;
        }
        let mut specs = plan.services;
        let spec = specs.remove(0);
        let metrics = RuntimeMetrics::new();
        let listener_metrics = metrics
            .register_listener(
                &spec.name,
                spec.kind.protocol(),
                spec.bind.to_string(),
                spec.max_connections.unwrap_or(u64::MAX),
            )
            .expect("proxy listener metrics");
        let ServiceKind::Http(service) = spec.kind else {
            panic!("configured listener must compile as HTTP");
        };
        let configuration = ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        };
        let configuration = Arc::new(configuration);
        let proxy = Arc::new(MonitoredHttpApp::new(
            http_proxy(
                &configuration,
                HttpReverseProxy::new(service, listener_metrics.clone()),
            ),
            listener_metrics,
        ));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut connections = Vec::with_capacity(expected_connections);
            for _ in 0..expected_connections {
                let (stream, _) = listener.accept().await.expect("proxy accept");
                let Some(admission) = proxy.admit_connection() else {
                    continue;
                };
                let stream: pingora::protocols::Stream =
                    Box::new(pingora::protocols::l4::stream::Stream::from(stream));
                let proxy = Arc::clone(&proxy);
                let shutdown = shutdown.clone();
                connections.push(tokio::spawn(async move {
                    let _admission = admission;
                    proxy.process_new(stream, &shutdown).await;
                }));
            }
            for connection in connections {
                connection.await.expect("proxy connection task");
            }
        });

        Self {
            address,
            metrics,
            pools,
            _shutdown_tx: shutdown_tx,
            task: Some(task),
        }
    }

    async fn request(&self, request_head: &str) -> RawResponse {
        self.request_bytes(format!("{request_head}Connection: close\r\n\r\n").as_bytes())
            .await
    }

    async fn request_bytes(&self, request: &[u8]) -> RawResponse {
        raw_http_request(self.address, request).await
    }

    async fn finish(mut self) {
        let task = self.task.take().expect("proxy task");
        task.await.expect("proxy task completed");
    }

    async fn wait_for_active_connections(&self, expected: u64) {
        timeout(Duration::from_secs(1), async {
            loop {
                if self
                    .metrics
                    .snapshot()
                    .expect("runtime snapshot")
                    .traffic
                    .active_connections
                    == expected
                {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("active connection count did not converge");
    }

    async fn wait_for_no_active_leases(&self) {
        timeout(Duration::from_secs(1), async {
            loop {
                if self.pools.iter().all(|pool| {
                    pool.health_snapshot()
                        .endpoints
                        .iter()
                        .all(|endpoint| endpoint.active_leases == 0)
                }) {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("active upstream leases did not drain");
    }
}

async fn unused_address() -> SocketAddr {
    unused_addresses(1).await[0]
}

async fn unused_addresses(count: usize) -> Vec<SocketAddr> {
    let mut listeners = Vec::with_capacity(count);
    for _ in 0..count {
        listeners.push(
            TcpListener::bind("127.0.0.1:0")
                .await
                .expect("unused address bind"),
        );
    }
    listeners
        .iter()
        .map(|listener| listener.local_addr().expect("unused address"))
        .collect()
}

impl Drop for ProxyHarness {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Origin {
    address: SocketAddr,
    accepted: Arc<AtomicUsize>,
    task: Option<JoinHandle<()>>,
}

impl Origin {
    async fn start(name: &'static str, expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("origin response");
                stream.shutdown().await.expect("origin shutdown");
            }
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    async fn start_status(status: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            accepted_by_task.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut stream)
                .await
                .expect("origin request");
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("origin response");
            stream.shutdown().await.expect("origin shutdown");
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    async fn start_silent() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            accepted_by_task.fetch_add(1, Ordering::SeqCst);
            let mut request = Vec::new();
            stream
                .read_to_end(&mut request)
                .await
                .expect("origin request close");
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    async fn start_disconnecting() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            accepted_by_task.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut stream)
                .await
                .expect("origin request");
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    async fn assert_not_contacted(mut self) {
        let contacted = timeout(NO_ORIGIN_CONTACT_WINDOW, async {
            while self.accepted.load(Ordering::SeqCst) == 0 {
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .is_ok();
        assert!(!contacted, "origin unexpectedly accepted a connection");
        self.task.take().expect("origin task").abort();
    }

    async fn finish(mut self) {
        let task = self.task.take().expect("origin task");
        task.await.expect("origin task completed");
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn read_request_head(stream: &mut TcpStream) -> io::Result<()> {
    read_request_head_from(stream).await
}

async fn read_request_head_from<S>(stream: &mut S) -> io::Result<()>
where
    S: AsyncRead + Unpin,
{
    read_request_head_bytes(stream).await.map(|_| ())
}

fn pool(name: &str, endpoints: &[SocketAddr]) -> UpstreamPool {
    endpoint_pool(
        name,
        endpoints.iter().copied().map(socket_endpoint).collect(),
        UpstreamAlgorithm::RoundRobin,
    )
}

fn endpoint_pool(
    name: &str,
    endpoints: Vec<UpstreamEndpoint>,
    algorithm: UpstreamAlgorithm,
) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        endpoints,
        algorithm,
        health_check: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
    }
}

fn route(host: Option<&str>, path: &str, methods: &[&str], pool: &str) -> HttpRoute {
    HttpRoute {
        host: host.map(|value| oxiroute_config::HttpHostSelector::NormalizedHost {
            value: value.into(),
        }),
        path: HttpPathSelector::SegmentPrefix { value: path.into() },
        methods: methods.iter().map(ToString::to_string).collect(),
        access_policy: None,
        action: HttpRouteAction::Proxy {
            upstream_pool: pool.into(),
            policy: HttpProxyPolicy::default(),
        },
    }
}

fn assert_origin_response(response: &RawResponse, origin: &str) {
    assert_eq!(response.status, 200, "response: {}", response.text());
    assert!(
        response.text().ends_with(origin),
        "expected response from {origin}, got: {}",
        response.text()
    );
}
