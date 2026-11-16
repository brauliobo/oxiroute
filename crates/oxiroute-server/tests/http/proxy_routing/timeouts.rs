use crate::TEST_TIMEOUT;
use crate::http_support::HttpResponse as RawResponse;
use crate::http_support::read_request_head as read_request_head_bytes;
use crate::support::builders::pool;
use crate::support::builders::proxy_policy_mut;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::Origin;
use crate::support::wire::assert_origin_response;
use crate::support::wire::read_request_head;
use oxiroute_config::DownstreamTimeoutPolicy;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpProxyTimeouts;
use oxiroute_config::HttpRetryTrigger;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use oxiroute_server::HttpOperationResult;
use std::net::SocketAddr;
use std::time::Duration;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::timeout;

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
        assert_eq!(timed_out.status, 504, "response: {}", timed_out.text());
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

fn timeout_route(timeouts: HttpProxyTimeouts) -> HttpRoute {
    let mut route = route(None, "/", &[], "origin");
    proxy_policy_mut(&mut route).timeouts = timeouts;
    route
}

pub(crate) async fn timed_get(proxy: &ProxyHarness, host: &str) -> (RawResponse, Duration) {
    let started = Instant::now();
    let response = proxy
        .request(&format!("GET / HTTP/1.1\r\nHost: {host}\r\n"))
        .await;
    (response, started.elapsed())
}

#[tokio::test]
async fn silent_upstream_past_the_service_deadline_answers_504_with_a_reason() {
    timeout(TEST_TIMEOUT, async {
        let silent = Origin::start_silent().await;
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[silent.address])],
            vec![route(None, "/", &[], "origin")],
            1024,
            1,
        )
        .await;

        let (response, elapsed) = timed_get(&proxy, "deadline.test").await;

        assert_eq!(response.status, 504, "response: {}", response.text());
        assert_eq!(response.header("x-oxiroute-reason"), Some("total_deadline"));
        assert!(elapsed < Duration::from_millis(1_800), "{elapsed:?}");
        proxy.finish().await;
        silent.finish().await;
    })
    .await
    .expect("service deadline test timed out");
}

#[tokio::test]
async fn route_total_timeout_shortens_the_service_deadline() {
    timeout(TEST_TIMEOUT, async {
        let silent = Origin::start_silent().await;
        let route = timeout_route(HttpProxyTimeouts {
            total_ms: Some(150),
            ..HttpProxyTimeouts::default()
        });
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[silent.address])],
            vec![route],
            1024,
            1,
        )
        .await;

        let (response, elapsed) = timed_get(&proxy, "total.test").await;

        assert_eq!(response.status, 504, "response: {}", response.text());
        assert_eq!(response.header("x-oxiroute-reason"), Some("total_deadline"));
        assert!(elapsed < Duration::from_millis(600), "{elapsed:?}");
        proxy.finish().await;
        silent.finish().await;
    })
    .await
    .expect("route total test timed out");
}

#[tokio::test]
async fn header_timeout_answers_504_and_leaves_budget_for_the_retry() {
    timeout(TEST_TIMEOUT, async {
        let first = Origin::start_silent().await;
        let second = Origin::start_silent().await;
        let healthy = Origin::start("retried-after-header-timeout", 1).await;
        let mut hung = timeout_route(HttpProxyTimeouts {
            header_ms: Some(100),
            ..HttpProxyTimeouts::default()
        });
        proxy_policy_mut(&mut hung).retry.triggers = vec![HttpRetryTrigger::ResponseTimeout];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("origin", &[first.address, second.address])],
            vec![hung.clone()],
            1,
            1,
        )
        .await;
        let (response, elapsed) = timed_get(&proxy, "header.test").await;
        assert_eq!(response.status, 504, "response: {}", response.text());
        assert_eq!(
            response.header("x-oxiroute-reason"),
            Some("upstream_header_timeout")
        );
        assert!(elapsed < Duration::from_millis(800), "{elapsed:?}");
        assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
        proxy.finish().await;

        let third = Origin::start_silent().await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("origin", &[third.address, healthy.address])],
            vec![hung],
            1,
            1,
        )
        .await;
        let (response, _) = timed_get(&proxy, "header.test").await;
        assert_origin_response(&response, "retried-after-header-timeout");
        proxy.finish().await;
    })
    .await
    .expect("header timeout test timed out");
}

#[tokio::test]
async fn route_header_timeout_takes_precedence_over_the_pool_server_timeout() {
    timeout(TEST_TIMEOUT, async {
        let silent = Origin::start_silent().await;
        let mut slow_pool = pool("origin", &[silent.address]);
        slow_pool.server_timeout_ms = Some(800);
        let route = timeout_route(HttpProxyTimeouts {
            header_ms: Some(100),
            ..HttpProxyTimeouts::default()
        });
        let proxy = ProxyHarness::start(vec![slow_pool], vec![route], 1024, 1).await;

        let (response, elapsed) = timed_get(&proxy, "precedence.test").await;

        assert_eq!(response.status, 504, "response: {}", response.text());
        assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
        proxy.finish().await;
    })
    .await
    .expect("timeout precedence test timed out");
}

#[tokio::test]
async fn pool_server_timeout_still_shadows_the_legacy_route_read_timeout() {
    timeout(TEST_TIMEOUT, async {
        let silent = Origin::start_silent().await;
        let mut slow_pool = pool("origin", &[silent.address]);
        slow_pool.server_timeout_ms = Some(300);
        let mut route = route(None, "/", &[], "origin");
        route.policy.read_timeout_ms = 20;
        let proxy = ProxyHarness::start(vec![slow_pool], vec![route], 1024, 1).await;

        let (response, elapsed) = timed_get(&proxy, "legacy.test").await;

        assert_eq!(response.status, 504, "response: {}", response.text());
        assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
        proxy.finish().await;
    })
    .await
    .expect("legacy timeout test timed out");
}

async fn stalling_origin(
    first_wait: Duration,
    partial: &'static [u8],
) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("stall bind");
    let address = listener.local_addr().expect("stall address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("stall accept");
        read_request_head(&mut stream).await.expect("stall request");
        sleep(first_wait).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n")
            .await;
        let _ = stream.write_all(partial).await;
        sleep(Duration::from_millis(1_500)).await;
    });
    (address, task)
}

#[tokio::test]
async fn idle_timeout_cuts_a_stalled_body_without_limiting_the_header_wait() {
    timeout(TEST_TIMEOUT, async {
        let timeouts = HttpProxyTimeouts {
            header_ms: Some(600),
            idle_ms: Some(100),
            ..HttpProxyTimeouts::default()
        };
        let (stalled, stalled_task) = stalling_origin(Duration::ZERO, b"abc").await;
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[stalled])],
            vec![timeout_route(timeouts)],
            1024,
            1,
        )
        .await;
        let started = Instant::now();
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("idle client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: idle.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("idle request");
        let mut received = Vec::new();
        let _ = timeout(Duration::from_secs(2), client.read_to_end(&mut received)).await;
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "{:?}",
            started.elapsed()
        );
        assert!(received.starts_with(b"HTTP/1.1 200"), "{received:?}");
        assert!(received.ends_with(b"abc"), "{received:?}");
        proxy.finish().await;
        stalled_task.abort();

        let (slow_start, slow_task) =
            stalling_origin(Duration::from_millis(250), b"0123456789").await;
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[slow_start])],
            vec![timeout_route(timeouts)],
            1024,
            1,
        )
        .await;
        let (response, _) = timed_get(&proxy, "idle.test").await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.body(), b"0123456789");
        proxy.finish().await;
        slow_task.abort();
    })
    .await
    .expect("idle timeout test timed out");
}

#[tokio::test]
async fn client_disconnect_cancels_the_pending_upstream_request() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let (closed_tx, closed_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            read_request_head(&mut stream)
                .await
                .expect("origin request");
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest).await;
            let _ = closed_tx.send(());
        });
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[address])],
            vec![route(None, "/", &[], "origin")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address).await.expect("client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: cancel.test\r\n\r\n")
            .await
            .expect("client request");
        sleep(Duration::from_millis(150)).await;
        drop(client);

        timeout(Duration::from_secs(30), closed_rx)
            .await
            .expect("origin did not observe the client disconnect")
            .expect("origin close signal");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
        origin.await.expect("origin task");
    })
    .await
    .expect("client disconnect test timed out");
}

#[tokio::test]
async fn total_request_deadline_cancels_a_silent_upstream_once() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("deadline origin bind");
        let address = listener.local_addr().expect("deadline origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("deadline origin accept");
            read_request_head_bytes(&mut stream)
                .await
                .expect("deadline origin request");
            sleep(Duration::from_secs(2)).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        });
        let proxy = ProxyHarness::start_with_features(
            vec![pool("deadline", &[address])],
            vec![route(None, "/", &[], "deadline")],
            Some(1024),
            true,
            100,
            0,
            false,
            1,
            None,
            None,
            DownstreamTimeoutPolicy {
                client_timeout_ms: Some(1_000),
                request_timeout_ms: Some(75),
                keepalive_timeout_ms: None,
            },
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("deadline client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: deadline.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("deadline request");
        let mut response = Vec::new();
        timeout(
            Duration::from_millis(300),
            client.read_to_end(&mut response),
        )
        .await
        .expect("total request deadline did not close the client")
        .expect("deadline client read");
        assert!(
            response.is_empty(),
            "unexpected late response: {response:?}"
        );

        let snapshot = proxy.metrics.snapshot().expect("deadline metrics snapshot");
        let operations = snapshot.listeners[0]
            .http_operations
            .as_ref()
            .expect("HTTP operation metrics");
        assert_eq!(
            operations
                .outcomes
                .iter()
                .find(|outcome| outcome.result == HttpOperationResult::Timeout)
                .map_or(0, |outcome| outcome.count),
            1
        );
        assert_eq!(operations.latency.count, 1);

        proxy.finish().await;
        origin.abort();
        let _ = origin.await;
    })
    .await
    .expect("total request deadline test timed out");
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
            true,
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
        let closed = timeout(Duration::from_secs(30), client.read(&mut byte))
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
            true,
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
        let closed = timeout(Duration::from_secs(30), client.read(&mut byte))
            .await
            .expect("idle keepalive closes");
        assert!(matches!(closed, Ok(0) | Err(_)), "keepalive remained open");

        proxy.finish().await;
    })
    .await
    .expect("keepalive timeout test timed out");
}
