#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;

use std::{
    fs, io,
    net::SocketAddr,
    os::fd::AsRawFd as _,
    os::unix::fs::symlink,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_config::{
    AccessLogPolicy, Config, DnsResolutionPolicy, DownstreamTimeoutPolicy, HealthCheck,
    HealthCheckType, HttpAccessPolicy, HttpCookieAttributePolicy, HttpCookiePathRewrite,
    HttpGzipPolicy, HttpLiteralHeader, HttpMimeType, HttpPathSelector, HttpProxyPolicy,
    HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryTarget, HttpRetryTrigger, HttpRoute, HttpRouteAction,
    HttpSameSite, HttpService, HttpStaticErrorResponse, HttpStaticMimePolicy,
    HttpStaticPathMapping, HttpStaticTryFile, HttpUpstreamHost, HttpVersionPolicy, Listener,
    Protocol, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use oxiroute_server::{
    HttpDownstreamPolicyApp, HttpReverseProxy, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RoundRobinPool,
    RuntimeMetrics, ServiceKind, runtime_plan,
};
use pingora::{
    apps::ServerApp,
    protocols::{GetSocketDigest as _, SocketDigest},
    proxy::http_proxy,
    server::configuration::ServerConf,
};
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
async fn whitebeast_shaped_nginx_suffix_routes_use_importer_resolved_first_wins_order_on_wire() {
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
    .expect("whitebeast suffix route test timed out");
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
async fn informational_responses_reach_final_and_reuse_upstream_connection() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("informational origin bind");
        let address = listener.local_addr().expect("informational origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("informational origin accept");
            let first_request = read_request_head_bytes(&mut stream)
                .await
                .expect("informational origin HEAD request");
            assert!(first_request.starts_with(b"HEAD /first HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nlInK: </style.css>; rel=preload\r\n\r\nHTTP/1.1 200 OK\r\ncOnTeNt-LeNgTh: 7\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .expect("informational origin responses");

            let second_request = read_request_head_bytes(&mut stream)
                .await
                .expect("reused informational origin request");
            assert!(second_request.starts_with(b"GET /second HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("reused informational origin response");
        });
        let proxy = ProxyHarness::start(
            vec![capped_pool("informational", address, 1)],
            vec![route(None, "/", &[], "informational")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("informational downstream connect");

        client
            .write_all(
                b"HEAD /first HTTP/1.1\r\nHost: informational.test\r\nExpect: 100-continue\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("informational downstream HEAD request");
        let continue_response = RawResponse::parse(
            read_response_head(&mut client)
                .await
                .expect("informational downstream response"),
        );
        assert_eq!(continue_response.status, 100);
        let informational = RawResponse::parse(
            read_response_head(&mut client)
                .await
                .expect("early hints downstream response"),
        );
        assert_eq!(informational.status, 103);
        assert!(informational.text().contains("\r\nlink: </style.css>"));
        assert!(!informational.text().contains("\r\nlInK:"));
        let final_response = RawResponse::parse(
            read_response_head(&mut client)
                .await
                .expect("final downstream HEAD response"),
        );
        assert_eq!(final_response.status, 200);
        assert!(final_response.body.is_empty());
        assert!(final_response.text().contains("\r\nContent-Length: 7\r\n"));
        assert!(!final_response.text().contains("cOnTeNt-LeNgTh"));

        client
            .write_all(
                b"GET /second HTTP/1.1\r\nHost: informational.test\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("reused downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("reused downstream response")
                .body,
            b"second"
        );
        drop(client);

        origin.await.expect("informational origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("informational HEAD response test timed out");
}

#[tokio::test]
async fn reusable_upstream_connection_holds_its_lease_until_the_socket_closes() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reusable origin bind");
        let address = listener.local_addr().expect("reusable origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("reusable origin accept");
            read_request_head(&mut stream)
                .await
                .expect("first reusable request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nfirst",
                )
                .await
                .expect("first reusable response");
            read_request_head(&mut stream)
                .await
                .expect("second reusable request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("second reusable response");
        });
        let proxy = ProxyHarness::start(
            vec![capped_pool("reused", address, 1)],
            vec![route(None, "/", &[], "reused")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("reusable downstream connect");

        client
            .write_all(b"GET / HTTP/1.1\r\nHost: reuse.test\r\n\r\n")
            .await
            .expect("first reusable downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("first reusable downstream response")
                .body,
            b"first"
        );
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0].active_connections,
            1,
            "an idle pooled socket must retain its physical connection lease"
        );
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: reuse.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("second reusable downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("second reusable downstream response")
                .body,
            b"second"
        );
        origin.await.expect("reusable origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("reusable upstream lease test timed out");
}

#[tokio::test]
async fn preread_one_kib_response_is_exact_and_keeps_upstream_reusable() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("preread origin bind");
        let address = listener.local_addr().expect("preread origin address");
        let expected_body = vec![b'x'; 1024];
        let origin_body = expected_body.clone();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("preread origin accept");
            read_request_head(&mut stream)
                .await
                .expect("first preread request");
            let mut response =
                b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: keep-alive\r\n\r\n"
                    .to_vec();
            response.extend_from_slice(&origin_body);
            stream
                .write_all(&response)
                .await
                .expect("single-write preread response");

            let second_request = read_request_head_bytes(&mut stream)
                .await
                .expect("reused preread request");
            assert!(second_request.starts_with(b"GET /second HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("reused preread response");
        });
        let proxy = ProxyHarness::start(
            vec![capped_pool("preread", address, 1)],
            vec![route(None, "/", &[], "preread")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("preread downstream connect");

        client
            .write_all(
                b"GET /first HTTP/1.1\r\nHost: preread.test\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("first preread downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("first preread downstream response")
                .body,
            expected_body
        );
        client
            .write_all(b"GET /second HTTP/1.1\r\nHost: preread.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("second preread downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("second preread downstream response")
                .body,
            b"second"
        );

        origin.await.expect("preread origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("preread response test timed out");
}

#[tokio::test]
async fn maxconn_one_serializes_concurrent_h1_creation_and_reuses_the_physical_connection() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("capped origin bind");
        let address = listener.local_addr().expect("capped origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_origin = Arc::clone(&accepted);
        let (first_seen_tx, first_seen_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("capped origin accept");
            accepted_by_origin.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut stream)
                .await
                .expect("first capped request");
            first_seen_tx.send(()).expect("first capped request signal");
            release_first_rx
                .await
                .expect("release first capped request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nfirst",
                )
                .await
                .expect("first capped response");
            read_request_head(&mut stream)
                .await
                .expect("second capped request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("second capped response");
        });
        let proxy = Arc::new(
            ProxyHarness::start(
                vec![capped_pool("capped", address, 1)],
                vec![route(None, "/", &[], "capped")],
                1024,
                2,
            )
            .await,
        );
        let first_proxy = Arc::clone(&proxy);
        let first =
            tokio::spawn(async move { keepalive_request(first_proxy.address, "/first").await });
        first_seen_rx.await.expect("first request reached origin");
        let second_proxy = Arc::clone(&proxy);
        let second =
            tokio::spawn(async move { keepalive_request(second_proxy.address, "/second").await });
        sleep(Duration::from_millis(50)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0].active_connections,
            1
        );
        release_first_tx.send(()).expect("release first response");
        assert_eq!(first.await.expect("first request task").body, b"first");
        assert_eq!(second.await.expect("second request task").body, b"second");
        origin.await.expect("capped origin task");
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        proxy.wait_for_no_active_leases().await;
        let proxy = Arc::into_inner(proxy).expect("proxy harness owner");
        proxy.finish().await;
    })
    .await
    .expect("concurrent capped H1 test timed out");
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
            startup: oxiroute_config::HealthStartup::default(),
            fast_interval_ms: None,
            down_interval_ms: None,
            host: None,
            path: None,
            expected_status: None,
            http_version: None,
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
async fn paused_upload_receives_early_final_response_without_spinning() {
    timeout(TEST_TIMEOUT, async {
        const ATTEMPTS: usize = 32;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("early-final origin bind");
        let address = listener.local_addr().expect("early-final origin address");
        let origin = tokio::spawn(async move {
            for attempt in 0..ATTEMPTS {
                let (mut stream, _) = listener.accept().await.expect("early-final accept");
                read_request_head_bytes(&mut stream)
                    .await
                    .expect("early-final request head");
                if attempt % 3 == 0 {
                    tokio::task::yield_now().await;
                }
                stream
                    .write_all(
                        b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .expect("early-final response");
            }
        });
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            2 * 1024 * 1024,
            ATTEMPTS,
        )
        .await;

        for attempt in 0..ATTEMPTS {
            let mut client = TcpStream::connect(proxy.address)
                .await
                .expect("early-final client");
            client
                .write_all(
                    b"POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\npartial",
                )
                .await
                .expect("partial upload");
            if attempt % 2 == 0 {
                tokio::task::yield_now().await;
            }
            let response = RawResponse::parse(
                read_response_head(&mut client)
                    .await
                    .expect("early-final downstream response"),
            );
            assert_eq!(response.status, 413);
        }

        origin.await.expect("early-final origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("paused upload early-final test timed out");
}

#[tokio::test]
async fn slow_downstream_preserves_response_larger_than_pipe_capacity() {
    timeout(TEST_TIMEOUT, async {
        const CHUNKS: usize = 64;
        const CHUNK_SIZE: usize = 512 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("slow-response origin bind");
        let address = listener.local_addr().expect("slow-response origin address");
        let (write_done_tx, mut write_done_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("slow-response accept");
            read_request_head_bytes(&mut stream)
                .await
                .expect("slow-response request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        CHUNKS * CHUNK_SIZE
                    )
                    .as_bytes(),
                )
                .await
                .expect("slow-response head");
            for value in 0..CHUNKS {
                let value = u8::try_from(value).expect("response chunk index fits in u8");
                stream
                    .write_all(&vec![value; CHUNK_SIZE])
                    .await
                    .expect("slow-response chunk");
                if value % 3 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            write_done_tx
                .send(())
                .expect("report completed origin write");
        });
        let proxy = ProxyHarness::start(
            vec![pool("slow", &[address])],
            vec![route(None, "/", &[], "slow")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("slow-response client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: slow.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("slow-response request write");
        sleep(Duration::from_millis(25)).await;
        assert!(
            timeout(Duration::from_millis(50), &mut write_done_rx)
                .await
                .is_err(),
            "origin did not block after the four-slot handoff and socket buffers filled"
        );

        let mut wire = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let read = client
                .read(&mut buffer)
                .await
                .expect("slow downstream read");
            if read == 0 {
                break;
            }
            wire.extend_from_slice(&buffer[..read]);
            tokio::task::yield_now().await;
        }
        let response = RawResponse::parse(wire);
        assert_eq!(response.status, 200);
        assert_eq!(response.body().len(), CHUNKS * CHUNK_SIZE);
        for (value, chunk) in response.body().chunks_exact(CHUNK_SIZE).enumerate() {
            let value = u8::try_from(value).expect("response chunk index fits in u8");
            assert!(chunk.iter().all(|byte| *byte == value));
        }
        timeout(Duration::from_secs(1), write_done_rx)
            .await
            .expect("origin did not resume after downstream reads")
            .expect("origin write completion sender dropped");

        origin.await.expect("slow-response origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("slow downstream response test timed out");
}

#[tokio::test]
async fn downstream_disconnect_releases_origin_blocked_by_response_backpressure() {
    timeout(TEST_TIMEOUT, async {
        const CHUNKS: usize = 1024;
        const CHUNK_SIZE: usize = 64 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("disconnect origin bind");
        let address = listener.local_addr().expect("disconnect origin address");
        let (started_tx, started_rx) = oneshot::channel();
        let (result_tx, mut result_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("disconnect origin accept");
            read_request_head_bytes(&mut stream)
                .await
                .expect("disconnect origin request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        CHUNKS * CHUNK_SIZE
                    )
                    .as_bytes(),
                )
                .await
                .expect("disconnect response head");
            started_tx.send(()).expect("report response start");
            let chunk = vec![b'x'; CHUNK_SIZE];
            let mut result = Ok(());
            for _ in 0..CHUNKS {
                if let Err(error) = stream.write_all(&chunk).await {
                    result = Err(error);
                    break;
                }
            }
            result_tx.send(result).expect("report origin write result");
        });
        let proxy = ProxyHarness::start(
            vec![pool("disconnect", &[address])],
            vec![route(None, "/", &[], "disconnect")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("disconnect client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: disconnect.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("disconnect request");
        started_rx.await.expect("origin started response");
        assert!(
            timeout(Duration::from_millis(50), &mut result_rx)
                .await
                .is_err(),
            "origin response completed before downstream disconnect"
        );
        drop(client);
        assert!(
            timeout(Duration::from_secs(2), result_rx)
                .await
                .expect("origin stayed blocked after downstream disconnect")
                .expect("origin result sender dropped")
                .is_err(),
            "origin unexpectedly wrote the complete response after disconnect"
        );

        origin.await.expect("disconnect origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("downstream disconnect backpressure test timed out");
}

#[tokio::test]
async fn saturated_upload_receives_early_final_then_observes_upstream_close() {
    timeout(TEST_TIMEOUT, async {
        const CHUNKS: usize = 512;
        const CHUNK_SIZE: usize = 64 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("saturated-upload origin bind");
        let address = listener.local_addr().expect("saturated-upload origin address");
        let (head_seen_tx, head_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("saturated-upload accept");
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.expect("saturated-upload head"));
            }
            head_seen_tx.send(()).expect("report upload head");
            release_rx.await.expect("release early final");
            stream
                .write_all(
                    b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("saturated-upload early final");
        });
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            64 * 1024 * 1024,
            1,
        )
        .await;
        let client = TcpStream::connect(proxy.address)
            .await
            .expect("saturated-upload client");
        let (mut client_read, mut client_write) = client.into_split();
        client_write
            .write_all(
                format!(
                    "POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    CHUNKS * CHUNK_SIZE
                )
                .as_bytes(),
            )
            .await
            .expect("saturated-upload request head");
        head_seen_rx.await.expect("origin observed request head");
        let mut writer = tokio::spawn(async move {
            let chunk = vec![b'u'; CHUNK_SIZE];
            for _ in 0..CHUNKS {
                client_write.write_all(&chunk).await?;
            }
            Ok::<_, io::Error>(())
        });
        assert!(
            timeout(Duration::from_millis(50), &mut writer)
                .await
                .is_err(),
            "upload completed without saturating request backpressure"
        );
        release_tx.send(()).expect("send early final");

        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            response.push(client_read.read_u8().await.expect("early-final response"));
        }
        assert_eq!(RawResponse::parse(response).status, 413);
        assert!(
            timeout(Duration::from_secs(1), writer)
                .await
                .expect("upload writer stayed blocked after upstream close")
                .expect("upload writer task panicked")
                .is_err(),
            "upload writer did not observe the closed proxy path"
        );

        origin.await.expect("saturated-upload origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("saturated upload early-final test timed out");
}

#[tokio::test]
async fn route_local_body_limits_are_independent_within_one_service() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("accepted", 1).await;
        let mut small = route(None, "/small", &["POST"], "upload");
        small.policy.max_request_body_bytes = Some(4);
        let mut larger = route(None, "/larger", &["POST"], "upload");
        larger.policy.max_request_body_bytes = Some(8);
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[origin.address])],
            vec![small, larger],
            1024,
            2,
        )
        .await;

        let rejected = proxy
            .request_bytes(
                b"POST /small HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345",
            )
            .await;
        assert_eq!(rejected.status, 413);
        let accepted = proxy
            .request_bytes(
                b"POST /larger HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345",
            )
            .await;
        assert_origin_response(&accepted, "accepted");

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("route-local body test timed out");
}

#[tokio::test]
async fn route_local_read_timeouts_are_independent() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("timeout origin bind");
        let origin_address = listener.local_addr().expect("timeout origin address");
        let origin = tokio::spawn(async move {
            for delayed in [true, false] {
                let (mut stream, _) = listener.accept().await.expect("timeout origin accept");
                read_request_head(&mut stream)
                    .await
                    .expect("timeout origin request");
                if delayed {
                    sleep(Duration::from_millis(100)).await;
                }
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });
        let mut short = route(None, "/short", &[], "origin");
        short.policy.read_timeout_ms = 20;
        let mut long = route(None, "/long", &[], "origin");
        long.policy.read_timeout_ms = 500;
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin_address])],
            vec![short, long],
            1024,
            2,
        )
        .await;

        let timed_out = proxy
            .request("GET /short HTTP/1.1\r\nHost: timeout.test\r\n")
            .await;
        assert_eq!(timed_out.status, 502, "response: {}", timed_out.text());
        let completed = proxy
            .request("GET /long HTTP/1.1\r\nHost: timeout.test\r\n")
            .await;
        assert_eq!(completed.status, 200, "response: {}", completed.text());

        proxy.finish().await;
        origin.await.expect("timeout origin task");
    })
    .await
    .expect("route timeout test timed out");
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
async fn delayed_same_server_retry_waits_and_reselects_the_original_server() {
    timeout(TEST_TIMEOUT, async {
        let reservation = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("late origin reservation");
        let address = reservation.local_addr().expect("late origin address");
        drop(reservation);
        let origin = tokio::spawn(async move {
            sleep(Duration::from_millis(75)).await;
            let listener = TcpListener::bind(address).await.expect("late origin bind");
            let (mut stream, _) = listener.accept().await.expect("late origin accept");
            read_request_head(&mut stream)
                .await
                .expect("late origin request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nlate")
                .await
                .expect("late origin response");
        });
        let mut retry_route = route(None, "/", &[], "retry");
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.target = HttpRetryTarget::SameServer;
        policy.retry.delay_ms = 200;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[address])],
            vec![retry_route],
            1,
            1,
        )
        .await;

        let started = Instant::now();
        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;
        assert_eq!(response.body, b"late");
        assert!(started.elapsed() >= Duration::from_millis(180));
        origin.await.expect("late origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("same-server retry test timed out");
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
async fn retries_a_post_before_any_bytes_are_sent_upstream() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let healthy = Origin::start("post-retry", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, healthy.address])],
            vec![route(None, "/", &[], "retry")],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("POST / HTTP/1.1\r\nHost: retry.test\r\nContent-Length: 0\r\n")
            .await;

        assert_origin_response(&response, "post-retry");
        proxy.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("unsafe retry test timed out");
}

#[tokio::test]
async fn retries_a_request_body_when_connection_failure_happens_before_send() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address().await;
        let healthy = Origin::start("body-retry", 1).await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, healthy.address])],
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

        assert_origin_response(&response, "body-retry");
        proxy.finish().await;
        healthy.finish().await;
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
#[expect(
    clippy::too_many_lines,
    reason = "one wire scenario covers fixed and redirect status/header semantics"
)]
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
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 200,
                        body: "fixed body".into(),
                        headers: vec![HttpLiteralHeader {
                            name: "x-fixed".into(),
                            value: "yes".into(),
                            always: true,
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
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 204,
                        body: String::new(),
                        headers: Vec::new(),
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/fixed-error".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 404,
                        body: "missing".into(),
                        headers: vec![
                            HttpLiteralHeader {
                                name: "x-selected-status".into(),
                                value: "no".into(),
                                always: false,
                            },
                            HttpLiteralHeader {
                                name: "x-always".into(),
                                value: "yes".into(),
                                always: true,
                            },
                        ],
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/redirect".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::Redirect {
                        status: 308,
                        location: HttpRedirectLocation::RequestTemplate {
                            value: "$scheme://$host$request_uri".into(),
                            nginx_host_fallback: None,
                        },
                        headers: vec![HttpLiteralHeader {
                            name: "x-redirect".into(),
                            value: "yes".into(),
                            always: false,
                        }],
                    },
                },
            ],
            1024,
            5,
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

        let fixed_error = proxy
            .request("GET /fixed-error HTTP/1.1\r\nHost: actions.example\r\n")
            .await;
        assert_eq!(fixed_error.status, 404);
        assert!(fixed_error.header("x-selected-status").is_none());
        assert_eq!(fixed_error.header("x-always"), Some("yes"));

        let redirect = proxy
            .request("GET /redirect?next=1 HTTP/1.1\r\nHost: Actions.Example:8080\r\n")
            .await;
        assert_eq!(redirect.status, 308);
        assert_eq!(
            redirect.header("location"),
            Some("http://actions.example/redirect?next=1")
        );
        assert_eq!(redirect.header("x-redirect"), Some("yes"));

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
            policy: oxiroute_config::HttpRoutePolicy::default(),
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
async fn basic_access_loads_only_bcrypt_htpasswd_files_without_exposing_credentials() {
    use std::os::unix::fs::PermissionsExt as _;

    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("htpasswd directory");
        let path = directory.path().join("users.htpasswd");
        fs::write(
            &path,
            b"myName:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
        )
        .expect("write htpasswd");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("secure htpasswd mode");
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: Some(HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path: path.clone(),
                realm: "private".into(),
            }),
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "authorized".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 4).await;

        let missing = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\n")
            .await;
        assert_eq!(
            missing.header("www-authenticate"),
            Some("Basic realm=\"private\", charset=\"UTF-8\"")
        );
        let wrong = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic bXlOYW1lOndyb25n\r\n")
            .await;
        assert_eq!(wrong.status, 401);
        let unknown = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic dW5rbm93bjp3cm9uZw==\r\n")
            .await;
        assert_eq!(unknown.status, 401);
        let accepted = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: bAsIc bXlOYW1lOm15UGFzc3dvcmQ=\r\n")
            .await;
        assert_eq!(accepted.status, 200);
        assert_eq!(accepted.body(), b"authorized");
        assert!(!accepted.text().contains("bXlOYW1l"));
        assert!(!accepted.text().contains(&path.display().to_string()));

        proxy.finish().await;
    })
    .await
    .expect("Basic htpasswd test timed out");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one adversarial scenario covers index, error, access, and loop rerouting"
)]
async fn nginx_internal_static_redirects_reselect_exact_routes_and_recheck_basic_access() {
    use std::os::unix::fs::PermissionsExt as _;

    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("internal redirect fixture");
        let root = create_secure_root(directory.path(), "public");
        fs::write(root.join("private.html"), b"private index").expect("private index");
        fs::write(root.join("50x.html"), b"private error").expect("private error");
        let htpasswd = directory.path().join("users.htpasswd");
        fs::write(
            &htpasswd,
            b"myName:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
        )
        .expect("write htpasswd");
        fs::set_permissions(&htpasswd, fs::Permissions::from_mode(0o600))
            .expect("secure htpasswd mode");
        let access = Some(HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path: htpasswd,
            realm: "private".into(),
        });
        let static_action = |indexes: Vec<String>, try_files, error_responses| {
            HttpRouteAction::StaticFiles {
                root_directory: root.clone(),
                path_mapping: HttpStaticPathMapping::Root,
                index_files: indexes,
                internal_index_redirects: true,
                directory_redirects: true,
                spa_fallback: None,
                try_files,
                autoindex: false,
                autoindex_exact_size: true,
                autoindex_local_time: false,
                mime: HttpStaticMimePolicy {
                    default_type: Some("text/plain".into()),
                    types: Vec::new(),
                },
                headers: Vec::new(),
                error_responses,
            }
        };
        let routes = vec![
            HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(vec!["private.html".into()], Vec::new(), Vec::new()),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/private.html".into(),
                },
                methods: Vec::new(),
                access_policy: access.clone(),
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(Vec::new(), Vec::new(), Vec::new()),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/error".into(),
                },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(
                    Vec::new(),
                    vec![HttpStaticTryFile::Status { status: 503 }],
                    vec![HttpStaticErrorResponse {
                        statuses: vec![503],
                        file: Some("50x.html".into()),
                        body: None,
                        headers: Vec::new(),
                        internal_redirect: Some("/50x.html".into()),
                    }],
                ),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/50x.html".into(),
                },
                methods: Vec::new(),
                access_policy: access,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(Vec::new(), Vec::new(), Vec::new()),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/loop".into(),
                },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(
                    Vec::new(),
                    vec![HttpStaticTryFile::Status { status: 503 }],
                    vec![HttpStaticErrorResponse {
                        statuses: vec![503],
                        file: Some("loop".into()),
                        body: None,
                        headers: Vec::new(),
                        internal_redirect: Some("/loop".into()),
                    }],
                ),
            },
        ];
        let proxy = ProxyHarness::start(Vec::new(), routes, 1024, 5).await;

        let index_denied = proxy.request("GET / HTTP/1.1\r\nHost: static.test\r\n").await;
        assert_eq!(index_denied.status, 401);
        let index = proxy
            .request("GET / HTTP/1.1\r\nHost: static.test\r\nAuthorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\n")
            .await;
        assert_eq!(index.status, 200);
        assert_eq!(index.body(), b"private index");

        let error_denied = proxy
            .request("GET /error HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(error_denied.status, 401);
        let error = proxy
            .request("GET /error HTTP/1.1\r\nHost: static.test\r\nAuthorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\n")
            .await;
        assert_eq!(error.status, 503);
        assert_eq!(error.body(), b"private error");

        let looped = proxy
            .request("GET /loop HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(looped.status, 500);

        proxy.finish().await;
    })
    .await
    .expect("internal redirect policy test timed out");
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
            .set_len(8 * 1024 * 1024 * 1024 + 1)
            .expect("oversized fixture length");
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::StaticFiles {
                root_directory: root.clone(),
                path_mapping: oxiroute_config::HttpStaticPathMapping::default(),
                index_files: vec!["index.html".into()],
                internal_index_redirects: false,
                directory_redirects: false,
                spa_fallback: Some("spa.html".into()),
                try_files: Vec::new(),
                autoindex: false,
                autoindex_exact_size: true,
                autoindex_local_time: false,
                mime: oxiroute_config::HttpStaticMimePolicy::default(),
                headers: Vec::new(),
                error_responses: Vec::new(),
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
#[expect(
    clippy::too_many_lines,
    reason = "one wire scenario covers the complete imported static policy"
)]
async fn host_shaped_static_policy_covers_root_alias_try_files_index_autoindex_mime_errors_and_ranges()
 {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("extended static fixture");
        let root = create_secure_root(directory.path(), "public");
        fs::write(root.join("asset.custom"), b"custom asset").expect("write custom asset");
        fs::write(root.join("fallback.txt"), b"fallback").expect("write fallback");
        fs::write(root.join("50x.html"), b"custom 50x").expect("write error document");
        fs::create_dir(root.join("directory")).expect("write index directory");
        fs::write(root.join("directory/index.html"), b"directory index")
            .expect("write directory index");
        fs::create_dir(root.join("empty-directory")).expect("write empty directory");
        fs::create_dir(root.join("root")).expect("write root mapping directory");
        fs::write(root.join("root/root.txt"), b"root mapped").expect("write root mapped file");
        fs::write(root.join("listing-entry.bin"), vec![b'x'; 1536])
            .expect("write listing entry");
        let large = fs::File::create(root.join("large.bin")).expect("write large fixture");
        large.set_len(17 * 1024 * 1024).expect("large fixture size");

        let proxy = ProxyHarness::start(
            Vec::new(),
            host_shaped_static_routes(&root),
            1024,
            21,
        )
        .await;

        let asset = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(asset.status, 200);
        assert_eq!(asset.header("content-type"), Some("application/x-custom"));
        assert_eq!(asset.header("x-static-policy"), Some("host-shaped"));
        assert_eq!(asset.body(), b"custom asset");
        let etag = asset.header("etag").expect("static ETag").to_owned();
        let last_modified = asset
            .header("last-modified")
            .expect("static Last-Modified")
            .to_owned();

        let failed_match = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-Match: \"stale\"\r\n")
            .await;
        assert_eq!(failed_match.status, 412);
        assert_eq!(failed_match.header("etag"), Some(etag.as_str()));

        let not_modified_since = proxy
            .request(&format!(
                "GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-Modified-Since: {last_modified}\r\n"
            ))
            .await;
        assert_eq!(not_modified_since.status, 304);
        assert_eq!(not_modified_since.header("etag"), Some(etag.as_str()));

        let failed_unmodified_since = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-Unmodified-Since: Thu, 01 Jan 1970 00:00:00 GMT\r\n")
            .await;
        assert_eq!(failed_unmodified_since.status, 412);

        let head_not_modified = proxy
            .request(&format!(
                "HEAD /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {etag}\r\n"
            ))
            .await;
        assert_eq!(head_not_modified.status, 304);
        assert!(head_not_modified.body().is_empty());

        let unsupported_method = proxy
            .request(&format!(
                "POST /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {etag}\r\nContent-Length: 0\r\n"
            ))
            .await;
        assert_eq!(unsupported_method.status, 405);

        let not_modified = proxy
            .request(&format!(
                "GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {etag}\r\n"
            ))
            .await;
        assert_eq!(not_modified.status, 304);
        assert!(not_modified.body().is_empty());

        let unknown_range = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nRange: widgets=0-1\r\n")
            .await;
        assert_eq!(unknown_range.status, 200);
        assert_eq!(unknown_range.body(), b"custom asset");

        let stale_if_range = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nRange: bytes=0-1\r\nIf-Range: \"stale\"\r\n")
            .await;
        assert_eq!(stale_if_range.status, 200);
        assert_eq!(stale_if_range.body(), b"custom asset");

        let matching_if_range = proxy
            .request(&format!(
                "GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nRange: bytes=0-1\r\nIf-Range: {etag}\r\n"
            ))
            .await;
        assert_eq!(matching_if_range.status, 206);
        assert_eq!(matching_if_range.body(), b"cu");
        assert!(matching_if_range.header("content-encoding").is_none());

        let directory_redirect = proxy
            .request("GET /assets/directory HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(directory_redirect.status, 301);
        assert_eq!(directory_redirect.header("location"), Some("/assets/directory/"));
        let index = proxy
            .request("GET /assets/directory/ HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(index.body(), b"directory index");

        let forbidden_directory = proxy
            .request("GET /assets/empty-directory HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(forbidden_directory.status, 301);
        let forbidden_directory = proxy
            .request("GET /assets/empty-directory/ HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(forbidden_directory.status, 403);

        let fallback = proxy
            .request("GET /fallback/missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(fallback.body(), b"fallback");

        let inline_error = proxy
            .request("GET /assets/missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(inline_error.status, 404);
        assert_eq!(inline_error.body(), b"branded missing");
        assert_eq!(inline_error.header("server"), Some("nginx/test"));

        let custom_error = proxy
            .request("GET /status/missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(custom_error.status, 503);
        assert_eq!(custom_error.body(), b"custom 50x");
        assert_eq!(custom_error.header("x-static-policy"), Some("host-shaped"));
        assert!(custom_error.header("x-nginx-status-policy").is_none());

        let listing = proxy
            .request("GET /listing/ HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(listing.status, 200);
        assert!(listing.text().contains("listing-entry.bin"));
        assert!(listing.text().contains("2K"));

        let ranged = proxy
            .request("GET /assets/large.bin HTTP/1.1\r\nHost: static.test\r\nRange: bytes=1048576-1048591\r\n")
            .await;
        assert_eq!(ranged.status, 206);
        assert_eq!(ranged.header("content-length"), Some("16"));
        assert_eq!(
            ranged.header("content-range"),
            Some("bytes 1048576-1048591/17825792")
        );
        assert_eq!(ranged.body(), &[0; 16]);

        let unsatisfiable = proxy
            .request("GET /assets/large.bin HTTP/1.1\r\nHost: static.test\r\nRange: bytes=99999999-\r\n")
            .await;
        assert_eq!(unsatisfiable.status, 416);
        assert_eq!(unsatisfiable.body(), b"custom 50x");
        assert_eq!(
            unsatisfiable.header("content-range"),
            Some("bytes */17825792")
        );
        assert_eq!(unsatisfiable.header("x-static-policy"), Some("host-shaped"));
        assert!(unsatisfiable.header("x-nginx-status-policy").is_none());

        let root_mapped = proxy
            .request("GET /root/root.txt HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(root_mapped.body(), b"root mapped");

        proxy.finish().await;
    })
    .await
    .expect("extended static policy test timed out");
}

fn host_shaped_static_routes(root: &std::path::Path) -> Vec<HttpRoute> {
    vec![
        host_shaped_static_route(
            root,
            "/assets",
            HttpStaticPathMapping::Alias,
            Vec::new(),
            false,
        ),
        host_shaped_static_route(
            root,
            "/fallback",
            HttpStaticPathMapping::Alias,
            vec![
                HttpStaticTryFile::RequestPath,
                HttpStaticTryFile::Relative {
                    path: "fallback.txt".into(),
                },
            ],
            false,
        ),
        host_shaped_static_route(
            root,
            "/status",
            HttpStaticPathMapping::Alias,
            vec![
                HttpStaticTryFile::RequestPath,
                HttpStaticTryFile::Status { status: 503 },
            ],
            false,
        ),
        host_shaped_static_route(
            root,
            "/listing",
            HttpStaticPathMapping::Alias,
            Vec::new(),
            true,
        ),
        host_shaped_static_route(
            root,
            "/root",
            HttpStaticPathMapping::Root,
            Vec::new(),
            false,
        ),
    ]
}

fn host_shaped_static_route(
    root: &std::path::Path,
    path: &str,
    mapping: HttpStaticPathMapping,
    try_files: Vec<HttpStaticTryFile>,
    autoindex: bool,
) -> HttpRoute {
    HttpRoute {
        host: None,
        path: HttpPathSelector::SegmentPrefix { value: path.into() },
        methods: Vec::new(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::StaticFiles {
            root_directory: root.to_owned(),
            path_mapping: mapping,
            index_files: vec!["index.html".into()],
            internal_index_redirects: true,
            directory_redirects: true,
            spa_fallback: None,
            try_files,
            autoindex,
            autoindex_exact_size: false,
            autoindex_local_time: true,
            mime: HttpStaticMimePolicy {
                default_type: Some("application/x-default".into()),
                types: vec![
                    HttpMimeType {
                        extension: "custom".into(),
                        content_type: "application/x-custom".into(),
                    },
                    HttpMimeType {
                        extension: "html".into(),
                        content_type: "text/html".into(),
                    },
                ],
            },
            headers: vec![
                HttpLiteralHeader {
                    name: "x-static-policy".into(),
                    value: "host-shaped".into(),
                    always: true,
                },
                HttpLiteralHeader {
                    name: "x-nginx-status-policy".into(),
                    value: "selected-statuses".into(),
                    always: false,
                },
            ],
            error_responses: vec![
                HttpStaticErrorResponse {
                    statuses: vec![416, 500, 502, 503, 504],
                    file: Some("50x.html".into()),
                    body: None,
                    headers: Vec::new(),
                    internal_redirect: None,
                },
                HttpStaticErrorResponse {
                    statuses: vec![404],
                    file: None,
                    body: Some("branded missing".into()),
                    headers: vec![HttpLiteralHeader {
                        name: "server".into(),
                        value: "nginx/test".into(),
                        always: true,
                    }],
                    internal_redirect: None,
                },
            ],
        },
    }
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
            assert!(request.contains("\r\nx-incoming: Client.Example.:8080\r\n"));
            assert!(request.contains("\r\nx-normalized: client.example.\r\n"));
            assert!(request.contains("\r\nx-nginx-host: client.example\r\n"));
            assert!(request.contains("\r\nx-selected: selected.example:9443\r\n"));
            assert!(request.contains("\r\nx-forwarded-for: trusted, 127.0.0.1\r\n"));
            assert!(request.contains("\r\nx-forwarded-proto: http\r\n"));
            assert!(request.contains("\r\nx-request-id: request-1\r\n"));
            assert!(request.contains("\r\nx-mixed: first\r\nx-mixed: second\r\n"));
            assert!(request.contains("\r\ncookie: a=1\r\ncookie: b=2\r\n"));
            assert!(!request.contains("X-MiXeD:"));
            assert!(!request.contains("CoOkIe:"));
            assert!(!request.contains("x-hop:"));
            assert!(!request.contains("x-remove:"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncOnTeNt-LeNgTh: 2\r\nX-Remove: old\r\nsEt-CoOkIe: sid=1; Path=/internal; HttpOnly\r\nSET-cookie: theme=dark; Path=/\r\nX-MiXeD: retained\r\nX-Hop: secret\r\nConnection: close\r\n\r\nok",
                )
                .await
                .expect("policy origin response");
        });
        let mut policy = host_shaped_proxy_policy();
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
                "GET / HTTP/1.1\r\nhOsT: Client.Example.:8080\r\nX-Remove: client\r\nX-Forwarded-For: trusted\r\nX-Request-Id: request-1\r\nX-MiXeD: first\r\nx-mIXed: second\r\nCoOkIe: a=1\r\nCOOKIE: b=2\r\nX-Hop: secret\r\n",
            )
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.header("x-added"), Some("new"));
        assert_eq!(response.header("x-remove"), None);
        assert_eq!(
            response.header("set-cookie"),
            Some("theme=dark; Path=/")
        );
        let response_wire = response.text();
        assert!(response_wire.contains(
            "\r\nSet-Cookie: sid=1; Path=/; Secure; SameSite=Lax\r\nSet-Cookie: theme=dark; Path=/\r\n"
        ));
        assert!(response_wire.contains("\r\nx-mixed: retained\r\n"));
        assert!(response_wire.contains("\r\nContent-Length: 2\r\n"));
        assert!(!response_wire.contains("X-MiXeD:"));
        assert!(!response_wire.contains("X-Hop:"));

        proxy.finish().await;
        origin.await.expect("policy origin task");
    })
    .await
    .expect("proxy policy test timed out");
}

#[tokio::test]
async fn x_forwarded_for_source_exception_preserves_the_incoming_header() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("XFF exception origin bind");
        let origin_address = listener.local_addr().expect("XFF exception origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("XFF origin accept");
            let request = String::from_utf8(
                read_request_head_bytes(&mut stream)
                    .await
                    .expect("XFF origin request"),
            )
            .expect("XFF request UTF-8");
            let request = request.to_ascii_lowercase();
            assert!(request.contains("\r\nx-forwarded-for: trusted\r\n"));
            assert!(!request.contains("trusted, 127.0.0.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .expect("XFF origin response");
        });
        let mut policy = host_shaped_proxy_policy();
        let HttpRequestHeaderMutation::Set { value, .. } = &mut policy.request_headers[4] else {
            panic!("XFF mutation")
        };
        let HttpRequestHeaderValue::AppendedXForwardedFor {
            except_source_cidrs,
            ..
        } = value
        else {
            panic!("XFF value")
        };
        except_source_cidrs.push("127.0.0.0/8".into());
        policy.retry.max_retries = 0;
        let mut proxy_route = route(None, "/", &[], "origin");
        let HttpRouteAction::Proxy {
            policy: route_policy,
            ..
        } = &mut proxy_route.action
        else {
            panic!("proxy route")
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
            .request("GET / HTTP/1.1\r\nHost: client.example\r\nX-Forwarded-For: trusted\r\n")
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        proxy.finish().await;
        origin.await.expect("XFF origin task");
    })
    .await
    .expect("XFF exception test timed out");
}

fn host_shaped_proxy_policy() -> HttpProxyPolicy {
    HttpProxyPolicy {
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
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-proto".into(),
                value: HttpRequestHeaderValue::DownstreamScheme,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-request-id".into(),
                value: HttpRequestHeaderValue::IncomingHeader {
                    name: "x-request-id".into(),
                    max_bytes: 32,
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-nginx-host".into(),
                value: HttpRequestHeaderValue::NginxHost {
                    fallback: "fallback.example".into(),
                },
            },
            HttpRequestHeaderMutation::Remove {
                name: "x-hop".into(),
            },
        ],
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "x-added".into(),
                value: "new".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-remove".into(),
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-hop".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/internal".into(),
            to: "/".into(),
        }],
        response_cookie_attributes: vec![HttpCookieAttributePolicy {
            name: "sid".into(),
            secure: Some(true),
            http_only: Some(false),
            same_site: Some(HttpSameSite::Lax),
        }],
        ..HttpProxyPolicy::default()
    }
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

#[tokio::test]
async fn configured_gzip_uses_pingora_streaming_only_for_exact_content_types() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("gzip origin bind");
        let origin_address = listener.local_addr().expect("gzip origin address");
        let origin = tokio::spawn(async move {
            for content_type in ["application/json", "text/plain"] {
                let (mut stream, _) = listener.accept().await.expect("gzip origin accept");
                read_request_head(&mut stream).await.expect("gzip request");
                let body = vec![b'a'; 512];
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.expect("gzip head");
                stream.write_all(&body).await.expect("gzip body");
            }
        });
        let proxy = ProxyHarness::start_with_features(
            vec![pool("origin", &[origin_address])],
            vec![route(None, "/", &[], "origin")],
            Some(1024),
            100,
            0,
            false,
            2,
            Some(HttpGzipPolicy {
                level: 6,
                content_types: vec!["application/json".into()],
            }),
            None,
            DownstreamTimeoutPolicy::default(),
        )
        .await;

        let compressed = proxy
            .request("GET /json HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(compressed.header("content-encoding"), Some("gzip"));
        assert_eq!(compressed.body().get(..2), Some(&[0x1f, 0x8b][..]));
        assert!(compressed.body().len() < 512);

        let plain = proxy
            .request("GET /text HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(plain.header("content-encoding"), None);
        assert_eq!(plain.body().len(), 512);

        proxy.finish().await;
        origin.await.expect("gzip origin task");
    })
    .await
    .expect("gzip policy test timed out");
}

#[tokio::test]
async fn structured_access_log_omits_authorization_cookies_and_query_tokens() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("access log directory");
        let path = directory.path().join("access.jsonl");
        let token = "0123456789abcdef0123456789abcdef";
        let token_path = write_secure_token(directory.path(), "access-log-token", token);
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::Exact {
                value: "/private".into(),
            },
            methods: Vec::new(),
            access_policy: Some(HttpAccessPolicy::BearerTokenFile {
                token_file_path: token_path,
                header_name: "authorization".into(),
                realm: None,
            }),
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 204,
                body: String::new(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start_with_features(
            Vec::new(),
            vec![route],
            Some(1024),
            100,
            0,
            false,
            4,
            None,
            Some(AccessLogPolicy::File { path: path.clone() }),
            DownstreamTimeoutPolicy::default(),
        )
        .await;

        let malformed = proxy
            .request("GET /private HTTP/1.1\r\nHost: first.test\r\nHost: second.test\r\n")
            .await;
        assert_eq!(malformed.status, 400);
        let unmatched = proxy
            .request("GET /missing HTTP/1.1\r\nHost: log.test\r\n")
            .await;
        assert_eq!(unmatched.status, 404);
        let unauthorized = proxy
            .request("GET /private HTTP/1.1\r\nHost: log.test\r\n")
            .await;
        assert_eq!(unauthorized.status, 401);
        let response = proxy
            .request(&format!(
                "GET /private?token=query-secret HTTP/1.1\r\nHost: log.test\r\nAuthorization: Bearer {token}\r\nCookie: session=cookie-secret\r\n"
            ))
            .await;
        assert_eq!(response.status, 204);
        proxy.finish().await;

        let contents = fs::read_to_string(path).expect("read access log");
        let events = contents
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("access event"))
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0]["route"], serde_json::Value::Null);
        assert_eq!(events[0]["status"], 400);
        assert_eq!(events[1]["route"], serde_json::Value::Null);
        assert_eq!(events[1]["status"], 404);
        assert_eq!(events[2]["route"], "0");
        assert_eq!(events[2]["status"], 401);
        assert_eq!(events[3]["service"], "routing");
        assert_eq!(events[3]["route"], "0");
        assert_eq!(events[3]["host"], "log.test");
        assert_eq!(events[3]["method"], "GET");
        assert_eq!(events[3]["status"], 204);
        for secret in ["query-secret", token, "cookie-secret", "Authorization", "Cookie"] {
            assert!(!contents.contains(secret), "access log exposed {secret}");
        }
    })
    .await
    .expect("access log test timed out");
}

#[tokio::test]
async fn listener_client_and_request_header_timeout_close_a_stalled_request() {
    timeout(TEST_TIMEOUT, async {
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "ok".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start_with_features(
            Vec::new(),
            vec![route],
            Some(1024),
            100,
            0,
            false,
            1,
            None,
            None,
            DownstreamTimeoutPolicy {
                client_timeout_ms: Some(30),
                request_timeout_ms: Some(30),
                keepalive_timeout_ms: Some(30),
            },
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("timeout client connect");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: stalled.test\r\nX-Incomplete:")
            .await
            .expect("partial request");
        sleep(Duration::from_millis(80)).await;
        let mut byte = [0; 1];
        let closed = timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .expect("stalled request closes");
        assert!(
            matches!(closed, Ok(0) | Err(_)),
            "stalled request remained open"
        );

        proxy.finish().await;
    })
    .await
    .expect("downstream timeout test timed out");
}

#[tokio::test]
async fn listener_keepalive_timeout_closes_an_idle_reusable_connection() {
    timeout(TEST_TIMEOUT, async {
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "ok".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start_with_features(
            Vec::new(),
            vec![route],
            Some(1024),
            100,
            0,
            false,
            1,
            None,
            None,
            DownstreamTimeoutPolicy {
                client_timeout_ms: Some(500),
                request_timeout_ms: Some(500),
                keepalive_timeout_ms: Some(30),
            },
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("keepalive client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: keepalive.test\r\nConnection: keep-alive\r\n\r\n")
            .await
            .expect("keepalive request");
        let mut response = Vec::new();
        let mut buffer = [0; 256];
        while !response.windows(4).any(|window| window == b"\r\n\r\n") || !response.ends_with(b"ok")
        {
            let read = client.read(&mut buffer).await.expect("keepalive response");
            assert!(read > 0);
            response.extend_from_slice(&buffer[..read]);
        }
        sleep(Duration::from_millis(15)).await;
        client.write_all(b"G").await.expect("active request byte");
        sleep(Duration::from_millis(80)).await;
        client
            .write_all(b"ET / HTTP/1.1\r\nHost: keepalive.test\r\nConnection: keep-alive\r\n\r\n")
            .await
            .expect("complete active request");
        let mut second_response = Vec::new();
        while !second_response
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
            || !second_response.ends_with(b"ok")
        {
            let read = client.read(&mut buffer).await.expect("active response");
            assert!(read > 0, "active request was closed by the idle timeout");
            second_response.extend_from_slice(&buffer[..read]);
        }
        sleep(Duration::from_millis(80)).await;
        let mut byte = [0; 1];
        let closed = timeout(Duration::from_secs(1), client.read(&mut byte))
            .await
            .expect("idle keepalive closes");
        assert!(matches!(closed, Ok(0) | Err(_)), "keepalive remained open");

        proxy.finish().await;
    })
    .await
    .expect("keepalive timeout test timed out");
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
        routes: Vec<HttpRoute>,
        max_request_body_bytes: Option<u64>,
        max_connections: u64,
        max_retries: u8,
        run_health_checks: bool,
        expected_connections: usize,
    ) -> Self {
        Self::start_with_features(
            upstream_pools,
            routes,
            max_request_body_bytes,
            max_connections,
            max_retries,
            run_health_checks,
            expected_connections,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "wire harness exposes canonical service policy"
    )]
    async fn start_with_features(
        upstream_pools: Vec<UpstreamPool>,
        mut routes: Vec<HttpRoute>,
        max_request_body_bytes: Option<u64>,
        max_connections: u64,
        max_retries: u8,
        run_health_checks: bool,
        expected_connections: usize,
        gzip: Option<HttpGzipPolicy>,
        access_log: Option<AccessLogPolicy>,
        downstream_timeouts: DownstreamTimeoutPolicy,
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
                downstream_timeouts,
            }],
            upstream_pools,
            http_services: vec![HttpService {
                name: "routing".into(),
                routes,
                upstream_io_timeout_ms: 1_000,
                max_request_body_bytes,
                gzip,
                access_log,
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
        let downstream_timeouts = spec.downstream_timeouts;
        let ServiceKind::Http(service) = spec.kind else {
            panic!("configured listener must compile as HTTP");
        };
        let configuration = ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        };
        let configuration = Arc::new(configuration);
        let proxy = Arc::new(MonitoredHttpApp::new(
            HttpDownstreamPolicyApp::new(
                http_proxy(
                    &configuration,
                    HttpReverseProxy::new(service, listener_metrics.clone()),
                ),
                downstream_timeouts,
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
                let mut stream = pingora::protocols::l4::stream::Stream::from(stream);
                stream.set_socket_digest(SocketDigest::from_raw_fd(stream.as_raw_fd()));
                let stream: pingora::protocols::Stream = Box::new(stream);
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
                        .all(|endpoint| endpoint.active_connections == 0)
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

async fn read_framed_response(stream: &mut TcpStream) -> io::Result<RawResponse> {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    let body_length = loop {
        if let Some(header_end) = response.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&response[..header_end])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Content-Length"))?;
            break (header_end + 4, length);
        }
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    };
    while response.len() < body_length.0 + body_length.1 {
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    }
    Ok(RawResponse::parse(response))
}

async fn read_response_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    }
    Ok(response)
}

async fn keepalive_request(address: SocketAddr, path: &str) -> RawResponse {
    let mut stream = TcpStream::connect(address)
        .await
        .expect("keepalive downstream connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: capped.test\r\nConnection: keep-alive\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("keepalive downstream request");
    read_framed_response(&mut stream)
        .await
        .expect("keepalive downstream response")
}

fn pool(name: &str, endpoints: &[SocketAddr]) -> UpstreamPool {
    endpoint_pool(
        name,
        endpoints.iter().copied().map(socket_endpoint).collect(),
        UpstreamAlgorithm::RoundRobin,
    )
}

fn capped_pool(name: &str, address: SocketAddr, max_connections: u64) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: vec![UpstreamServer {
            name: "origin".into(),
            endpoint: socket_endpoint(address),
            max_connections: Some(max_connections),
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: Some(1_000),
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
    }
}

fn endpoint_pool(
    name: &str,
    endpoints: Vec<UpstreamEndpoint>,
    algorithm: UpstreamAlgorithm,
) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: Vec::new(),
        endpoints,
        algorithm,
        health_check: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
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
        policy: oxiroute_config::HttpRoutePolicy::default(),
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
