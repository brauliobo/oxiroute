use crate::TEST_TIMEOUT;
use crate::config_support::socket_endpoint;
use crate::support::builders::endpoint_pool;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::harness::unlistened_socket;
use crate::support::harness::unused_address;
use crate::support::harness::unused_addresses;
use crate::support::origin::Origin;
use crate::support::wire::assert_origin_response;
use crate::support::wire::read_request_head;
use oxiroute_config::HttpRetryBodySafety;
use oxiroute_config::HttpRetryMethodSafety;
use oxiroute_config::HttpRetryTarget;
use oxiroute_config::HttpRetryTrigger;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::UpstreamAlgorithm;
use oxiroute_config::UpstreamPool;
use oxiroute_server::AdministrativeState;
use oxiroute_server::MAX_HTTP_ATTEMPTS;
use std::net::SocketAddr;
use std::time::Duration;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::sleep;
use tokio::time::timeout;

#[tokio::test]
async fn retries_a_bodyless_get_on_a_distinct_endpoint_after_connect_failure() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address();
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
async fn retries_a_bodyless_get_on_a_configured_origin_status() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = Origin::start_status(503, "unavailable").await;
        let healthy = Origin::start("status-retry-success", 1).await;
        let mut retry_route = route(None, "/", &[], "retry");
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.response_statuses = vec![503];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable.address, healthy.address])],
            vec![retry_route],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_origin_response(&response, "status-retry-success");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
        unavailable.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("origin status retry test timed out");
}

#[tokio::test]
async fn preserves_the_origin_status_when_status_retry_has_no_target() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_status(503, "terminal-origin").await;
        let mut retry_route = route(None, "/", &[], "retry");
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.response_statuses = vec![503];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[origin.address])],
            vec![retry_route],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_eq!(response.status, 503, "response: {}", response.text());
        assert_eq!(response.body(), b"terminal-origin");
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("terminal origin status test timed out");
}

#[tokio::test]
async fn delayed_same_server_retry_waits_and_reselects_the_original_server() {
    timeout(TEST_TIMEOUT, async {
        let reservation = unlistened_socket();
        let address = reservation.local_addr().expect("late origin address");
        let origin = tokio::spawn(async move {
            sleep(Duration::from_millis(75)).await;
            let listener = reservation.listen(16).expect("late origin listen");
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
async fn final_redispatch_retries_same_server_then_selects_an_available_server() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address();
        let healthy = Origin::start("redispatched", 1).await;
        let mut retry_route = route(None, "/", &[], "retry");
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.target = HttpRetryTarget::SameServer;
        policy.retry.delay_ms = 200;
        policy.retry.final_redispatch = true;
        policy.retry.triggers = vec![
            HttpRetryTrigger::ConnectFailure,
            HttpRetryTrigger::ConnectTimeout,
        ];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, healthy.address])],
            vec![retry_route],
            3,
            1,
        )
        .await;

        let started = Instant::now();
        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_origin_response(&response, "redispatched");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(350),
            "elapsed: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_millis(550), "elapsed: {elapsed:?}");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("final redispatch test timed out");
}

#[test]
fn pingora_attempt_cap_covers_four_route_attempts_with_sixteen_dns_addresses_each() {
    assert_eq!(MAX_HTTP_ATTEMPTS, 4 * 16);
}

#[tokio::test]
async fn retries_a_bodyless_head_after_connect_failure() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address();
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
        let unavailable = unused_address();
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
        let unavailable = unused_address();
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
        let unavailable = unused_address();
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
async fn retries_when_an_upstream_closes_before_sending_a_response() {
    timeout(TEST_TIMEOUT, async {
        let disconnecting = Origin::start_disconnecting().await;
        let healthy = Origin::start("empty-response-retry", 1).await;
        let mut retry_route = route(None, "/", &[], "retry");
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.triggers = vec![HttpRetryTrigger::EmptyResponse];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[disconnecting.address, healthy.address])],
            vec![retry_route],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_origin_response(&response, "empty-response-retry");
        proxy.finish().await;
        disconnecting.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("empty response retry test timed out");
}

#[tokio::test]
async fn retries_when_an_upstream_response_times_out() {
    timeout(Duration::from_secs(6), async {
        let silent = Origin::start_silent().await;
        let healthy = Origin::start("response-timeout-retry", 1).await;
        let mut retry_route = route(None, "/", &[], "retry");
        retry_route.policy.read_timeout_ms = 50;
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.triggers = vec![HttpRetryTrigger::ResponseTimeout];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[silent.address, healthy.address])],
            vec![retry_route],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_origin_response(&response, "response-timeout-retry");
        proxy.finish().await;
        silent.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("response timeout retry test timed out");
}

#[tokio::test]
async fn retries_a_buffered_json_post_after_an_upstream_response_timeout() {
    timeout(Duration::from_secs(6), async {
        let silent = Origin::start_silent().await;
        let healthy_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("healthy origin bind");
        let healthy_address = healthy_listener
            .local_addr()
            .expect("healthy origin address");
        let expected_body = b"{\"message\":\"retry\"}";
        let healthy = tokio::spawn(async move {
            let (mut stream, _) = healthy_listener.accept().await.expect("healthy origin accept");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = stream.read(&mut chunk).await.expect("healthy origin request");
                assert_ne!(read, 0, "healthy origin request ended early");
                request.extend_from_slice(&chunk[..read]);
                if request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .is_some_and(|header_end| request.len() >= header_end + 4 + expected_body.len())
                {
                    break;
                }
            }
            assert!(request.starts_with(b"POST /v1/chat HTTP/1.1\r\n"));
            assert!(request.ends_with(expected_body), "request: {request:?}");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nreplayed")
                .await
                .expect("healthy origin response");
        });
        let mut retry_route = route(None, "/", &[], "retry");
        retry_route.policy.read_timeout_ms = 50;
        retry_route.policy.request_buffering = true;
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.triggers = vec![HttpRetryTrigger::ResponseTimeout];
        policy.retry.method_safety = HttpRetryMethodSafety::All;
        policy.retry.body_safety = HttpRetryBodySafety::Buffered;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[silent.address, healthy_address])],
            vec![retry_route],
            1,
            1,
        )
        .await;

        let request = [
            format!(
                "POST /v1/chat HTTP/1.1\r\nHost: retry.test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                expected_body.len()
            )
            .into_bytes(),
            expected_body.to_vec(),
        ]
        .concat();
        let response = proxy.request_bytes(&request).await;

        assert_origin_response(&response, "replayed");
        healthy.await.expect("healthy origin task");
        proxy.finish().await;
        silent.finish().await;
    })
    .await
    .expect("buffered JSON response timeout retry test timed out");
}

#[tokio::test]
async fn does_not_retry_a_default_json_post_after_an_upstream_response_timeout() {
    timeout(Duration::from_secs(6), async {
        let silent = Origin::start_silent().await;
        let unused = Origin::start("must-not-receive", 1).await;
        let mut retry_route = route(None, "/", &[], "retry");
        retry_route.policy.read_timeout_ms = 50;
        let HttpRouteAction::Proxy { policy, .. } = &mut retry_route.action else {
            panic!("proxy route");
        };
        policy.retry.triggers = vec![HttpRetryTrigger::ResponseTimeout];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[silent.address, unused.address])],
            vec![retry_route],
            1,
            1,
        )
        .await;

        let response = proxy
            .request_bytes(
                b"POST /v1/chat HTTP/1.1\r\nHost: retry.test\r\nContent-Type: application/json\r\nContent-Length: 19\r\nConnection: close\r\n\r\n{\"message\":\"retry\"}",
            )
            .await;

        assert_eq!(response.status, 504, "response: {}", response.text());
        proxy.finish().await;
        silent.finish().await;
        unused.assert_not_contacted().await;
    })
    .await
    .expect("default JSON response timeout retry test timed out");
}

#[tokio::test]
async fn does_not_retry_an_upgrade_request() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address();
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

const TTS_POST: &[u8] = b"POST /tts HTTP/1.1\r\nHost: cluster.test\r\nContent-Type: application/json\r\nContent-Length: 19\r\nConnection: close\r\n\r\n{\"message\":\"retry\"}";

/// A node whose only local worker pool is fully drained, like `oxiroute server drain` on every server.
async fn drained_node(worker: &Origin) -> ProxyHarness {
    let node = ProxyHarness::start(
        vec![UpstreamPool {
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::Never,
            ..pool("workers", &[worker.address])
        }],
        vec![route(None, "/", &[], "workers")],
        1024,
        1,
    )
    .await;
    node.pools[0].set_administrative_state(AdministrativeState::Drain);
    node
}

fn first_cluster(nodes: &[SocketAddr]) -> UpstreamPool {
    UpstreamPool {
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Never,
        ..endpoint_pool(
            "nodes",
            nodes.iter().copied().map(socket_endpoint).collect(),
            UpstreamAlgorithm::First,
        )
    }
}

#[tokio::test]
async fn redispatches_a_post_from_a_node_with_every_local_server_drained_to_the_next_cluster_node()
{
    timeout(TEST_TIMEOUT, async {
        let worker = Origin::start("must-not-receive", 1).await;
        let drained = drained_node(&worker).await;
        let healthy = Origin::start("node-b", 1).await;
        let cluster = ProxyHarness::start_with_retries(
            vec![first_cluster(&[drained.address, healthy.address])],
            vec![route(None, "/", &[], "nodes")],
            1,
            1,
        )
        .await;

        let response = cluster.request_bytes(TTS_POST).await;

        assert_origin_response(&response, "node-b");
        cluster.finish().await;
        drained.finish().await;
        healthy.finish().await;
        worker.assert_not_contacted().await;
    })
    .await
    .expect("drained node redispatch test timed out");
}

#[tokio::test]
async fn answers_503_when_every_cluster_node_has_every_local_server_drained() {
    timeout(TEST_TIMEOUT, async {
        let workers = [
            Origin::start("must-not-receive", 1).await,
            Origin::start("must-not-receive", 1).await,
        ];
        let first = drained_node(&workers[0]).await;
        let second = drained_node(&workers[1]).await;
        let cluster = ProxyHarness::start_with_retries(
            vec![first_cluster(&[first.address, second.address])],
            vec![route(None, "/", &[], "nodes")],
            1,
            1,
        )
        .await;

        let response = cluster.request_bytes(TTS_POST).await;

        assert_eq!(response.status, 503, "response: {}", response.text());
        assert_eq!(response.header("x-oxiroute-pool-unavailable"), Some("1"));
        cluster.finish().await;
        first.finish().await;
        second.finish().await;
        for worker in workers {
            worker.assert_not_contacted().await;
        }
    })
    .await
    .expect("all drained nodes test timed out");
}

#[tokio::test]
async fn stops_when_the_configured_retry_budget_is_exhausted() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_addresses(2);
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
        let unavailable = unused_addresses(2);
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
async fn retries_only_for_a_configured_trigger() {
    timeout(TEST_TIMEOUT, async {
        let unavailable = unused_address();
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
