use crate::TEST_TIMEOUT;
use crate::config_support::socket_endpoint;
use crate::support::builders::endpoint_pool;
use crate::support::builders::pool;
use crate::support::builders::proxy_policy_mut;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::harness::unused_address;
use crate::support::origin::Origin;
use crate::support::origin::StatusOrigin;
use crate::support::wire::assert_origin_response;
use crate::timeouts::timed_get;
use oxiroute_config::HttpRescuePolicy;
use oxiroute_config::HttpRescueTarget;
use oxiroute_config::HttpRetryBodySafety;
use oxiroute_config::HttpRetryBudget;
use oxiroute_config::HttpRetryMethodSafety;
use oxiroute_config::HttpRetryTarget;
use oxiroute_config::HttpRetryTrigger;
use oxiroute_config::HttpRoute;
use oxiroute_config::UpstreamAlgorithm;
use oxiroute_config::UpstreamPool;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::sleep;
use tokio::time::timeout;

fn rescue_route(statuses: &[u16], target: HttpRescueTarget, pool_name: Option<&str>) -> HttpRoute {
    let mut route = route(None, "/", &[], "primary");
    proxy_policy_mut(&mut route).rescue = Some(HttpRescuePolicy {
        statuses: statuses.to_vec(),
        target,
        pool: pool_name.map(str::to_owned),
    });
    route
}

fn buffered_post_replay(route: &mut HttpRoute) {
    route.policy.request_buffering = true;
    let retry = &mut proxy_policy_mut(route).retry;
    retry.method_safety = HttpRetryMethodSafety::All;
    retry.body_safety = HttpRetryBodySafety::Buffered;
}

const JSON_POST: &[u8] = b"POST /api/chat HTTP/1.1\r\nHost: rescue.test\r\nContent-Type: application/json\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"model\":\"qwen\"}";

#[tokio::test]
async fn rescue_replays_a_rescued_status_on_the_other_server() {
    timeout(TEST_TIMEOUT, async {
        let missing = StatusOrigin::start(404, "missing-model").await;
        let healthy = StatusOrigin::start(200, "has-model").await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("primary", &[missing.address, healthy.address])],
            vec![rescue_route(&[404], HttpRescueTarget::NextServer, None)],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET /api/tags HTTP/1.1\r\nHost: rescue.test\r\n")
            .await;

        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.body(), b"has-model");
        assert_eq!((missing.hits(), healthy.hits()), (1, 1));
        proxy.wait_for_no_active_leases().await;
        let rescues = proxy.metrics.snapshot().expect("metrics").process.rescues;
        assert_eq!(rescues.len(), 1);
        assert_eq!(
            (
                rescues[0].route.as_str(),
                rescues[0].status,
                rescues[0].count
            ),
            ("0", 404, 1)
        );
        proxy.finish().await;
    })
    .await
    .expect("rescue test timed out");
}

#[tokio::test]
async fn rescue_delivers_the_last_backend_response_unchanged_when_every_server_answers_it() {
    timeout(TEST_TIMEOUT, async {
        let first = StatusOrigin::start(404, "first-404").await;
        let second = StatusOrigin::start(404, "second-404").await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("primary", &[first.address, second.address])],
            vec![rescue_route(&[404], HttpRescueTarget::NextServer, None)],
            3,
            1,
        )
        .await;

        let response = proxy
            .request("GET /api/tags HTTP/1.1\r\nHost: rescue.test\r\n")
            .await;

        assert_eq!(response.status, 404, "response: {}", response.text());
        assert_eq!(response.header("x-origin"), Some("second-404"));
        assert_eq!(response.body(), b"second-404");
        assert_eq!(
            (first.hits(), second.hits()),
            (1, 1),
            "each server at most once"
        );
        proxy.finish().await;
    })
    .await
    .expect("rescue exhaustion test timed out");
}

#[tokio::test]
async fn rescue_spends_the_retry_budget() {
    timeout(TEST_TIMEOUT, async {
        let origins = [
            StatusOrigin::start(404, "one").await,
            StatusOrigin::start(404, "two").await,
            StatusOrigin::start(404, "three").await,
        ];
        let addresses = origins
            .iter()
            .map(|origin| origin.address)
            .collect::<Vec<_>>();
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("primary", &addresses)],
            vec![rescue_route(&[404], HttpRescueTarget::NextServer, None)],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: rescue.test\r\n")
            .await;

        assert_eq!(response.status, 404);
        assert_eq!(origins.iter().map(StatusOrigin::hits).sum::<usize>(), 2);
        proxy.finish().await;
    })
    .await
    .expect("rescue budget test timed out");
}

#[tokio::test]
async fn rescue_leaves_unlisted_statuses_alone() {
    timeout(TEST_TIMEOUT, async {
        let failing = StatusOrigin::start(500, "failing").await;
        let spare = StatusOrigin::start(200, "spare").await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("primary", &[failing.address, spare.address])],
            vec![rescue_route(&[404], HttpRescueTarget::NextServer, None)],
            1,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: rescue.test\r\n")
            .await;

        assert_eq!(response.status, 500);
        assert_eq!(spare.hits(), 0);
        proxy.finish().await;
    })
    .await
    .expect("unlisted status test timed out");
}

#[tokio::test]
async fn rescue_replays_a_buffered_post_with_its_body() {
    timeout(TEST_TIMEOUT, async {
        let missing = StatusOrigin::start(404, "missing-model").await;
        let healthy = StatusOrigin::start(200, "has-model").await;
        let mut route = rescue_route(&[404], HttpRescueTarget::NextServer, None);
        buffered_post_replay(&mut route);
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("primary", &[missing.address, healthy.address])],
            vec![route],
            1,
            1,
        )
        .await;

        let response = proxy.request_bytes(JSON_POST).await;

        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(
            healthy.bodies.lock().expect("bodies").as_slice(),
            [b"{\"model\":\"qwen\"}".to_vec()]
        );
        proxy.finish().await;
    })
    .await
    .expect("buffered rescue test timed out");
}

#[tokio::test]
async fn rescue_does_not_replay_a_post_outside_the_method_and_body_safety_rules() {
    timeout(TEST_TIMEOUT, async {
        let missing = StatusOrigin::start(404, "missing-model").await;
        let healthy = StatusOrigin::start(200, "has-model").await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("primary", &[missing.address, healthy.address])],
            vec![rescue_route(&[404], HttpRescueTarget::NextServer, None)],
            1,
            1,
        )
        .await;

        let response = proxy.request_bytes(JSON_POST).await;

        assert_eq!(response.status, 404);
        assert_eq!(response.body(), b"missing-model");
        assert_eq!(healthy.hits(), 0);
        proxy.finish().await;
    })
    .await
    .expect("unsafe rescue test timed out");
}

#[tokio::test]
async fn rescue_can_replay_on_a_named_pool() {
    timeout(TEST_TIMEOUT, async {
        let missing = StatusOrigin::start(404, "missing-model").await;
        let spare = StatusOrigin::start(200, "spare-pool").await;
        let proxy = ProxyHarness::start_with_retries(
            vec![
                pool("primary", &[missing.address]),
                pool("spare", &[spare.address]),
            ],
            vec![rescue_route(&[404], HttpRescueTarget::Pool, Some("spare"))],
            1,
            1,
        )
        .await;

        let rescued = proxy
            .request("GET / HTTP/1.1\r\nHost: rescue.test\r\n")
            .await;
        assert_eq!(rescued.status, 200, "response: {}", rescued.text());
        assert_eq!(rescued.body(), b"spare-pool");
        assert_eq!((missing.hits(), spare.hits()), (1, 1));
        proxy.finish().await;

        let still_missing = StatusOrigin::start(404, "spare-missing").await;
        let proxy = ProxyHarness::start_with_retries(
            vec![
                pool("primary", &[missing.address]),
                pool("spare", &[still_missing.address]),
            ],
            vec![rescue_route(&[404], HttpRescueTarget::Pool, Some("spare"))],
            1,
            1,
        )
        .await;
        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: rescue.test\r\n")
            .await;
        assert_eq!(response.status, 404);
        assert_eq!(response.body(), b"spare-missing");
        proxy.finish().await;
    })
    .await
    .expect("rescue pool test timed out");
}

fn first_pool(name: &str, endpoints: &[SocketAddr]) -> UpstreamPool {
    endpoint_pool(
        name,
        endpoints.iter().copied().map(socket_endpoint).collect(),
        UpstreamAlgorithm::First,
    )
}

fn status_retry_route(statuses: &[u16]) -> HttpRoute {
    let mut route = route(None, "/", &[], "retry");
    proxy_policy_mut(&mut route).retry.response_statuses = statuses.to_vec();
    route
}

fn json_post_with(headers: &str) -> Vec<u8> {
    format!(
        "POST /api/chat HTTP/1.1\r\nHost: retry.test\r\nContent-Length: 16\r\nConnection: close\r\n{headers}\r\n{{\"model\":\"qwen\"}}"
    )
    .into_bytes()
}

#[tokio::test]
async fn idempotency_key_gates_the_replay_of_a_post_after_a_response_timeout() {
    timeout(Duration::from_secs(8), async {
        for (headers, replayed) in [("", false), ("Idempotency-Key: job-1\r\n", true)] {
            let silent = Origin::start_silent().await;
            let healthy = StatusOrigin::start(200, "replayed").await;
            let mut route = status_retry_route(&[]);
            route.policy.read_timeout_ms = 100;
            buffered_post_replay(&mut route);
            let retry = &mut proxy_policy_mut(&mut route).retry;
            retry.triggers = vec![HttpRetryTrigger::ResponseTimeout];
            retry.idempotency_header = Some("idempotency-key".into());
            let proxy = ProxyHarness::start_with_retries(
                vec![first_pool("retry", &[silent.address, healthy.address])],
                vec![route],
                1,
                1,
            )
            .await;

            let response = proxy.request_bytes(&json_post_with(headers)).await;

            if replayed {
                assert_eq!(response.status, 200, "response: {}", response.text());
                assert_eq!(response.body(), b"replayed");
            } else {
                assert_eq!(response.status, 504, "response: {}", response.text());
            }
            assert_eq!(healthy.hits(), usize::from(replayed));
            proxy.finish().await;
            silent.finish().await;
        }
    })
    .await
    .expect("idempotency key test timed out");
}

#[tokio::test]
async fn retry_budget_per_second_stops_retries_until_the_next_second() {
    timeout(Duration::from_secs(8), async {
        let failing = StatusOrigin::start(500, "failing").await;
        let healthy = StatusOrigin::start(200, "healthy").await;
        let mut route = status_retry_route(&[500]);
        proxy_policy_mut(&mut route).retry.budget = Some(HttpRetryBudget {
            per_second: Some(1),
            ratio_percent: None,
        });
        let proxy = ProxyHarness::start_with_retries(
            vec![first_pool("retry", &[failing.address, healthy.address])],
            vec![route],
            1,
            3,
        )
        .await;
        let get = "GET / HTTP/1.1\r\nHost: retry.test\r\n";

        assert_eq!(proxy.request(get).await.status, 200);
        let exhausted = proxy.request(get).await;
        assert_eq!(exhausted.status, 500, "response: {}", exhausted.text());
        assert_eq!(exhausted.body(), b"failing");
        assert_eq!(healthy.hits(), 1);
        sleep(Duration::from_millis(1_100)).await;
        assert_eq!(proxy.request(get).await.status, 200);
        assert_eq!(healthy.hits(), 2);
        proxy.finish().await;
    })
    .await
    .expect("per second budget test timed out");
}

#[tokio::test]
async fn retry_budget_ratio_limits_retries_to_a_share_of_the_requests() {
    timeout(Duration::from_secs(8), async {
        let failing = StatusOrigin::start(500, "failing").await;
        let healthy = StatusOrigin::start(200, "healthy").await;
        let mut route = status_retry_route(&[500]);
        proxy_policy_mut(&mut route).retry.budget = Some(HttpRetryBudget {
            per_second: None,
            ratio_percent: Some(20),
        });
        let proxy = ProxyHarness::start_with_retries(
            vec![first_pool("retry", &[failing.address, healthy.address])],
            vec![route],
            1,
            10,
        )
        .await;

        let mut statuses = Vec::new();
        for _ in 0..10 {
            statuses.push(
                proxy
                    .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
                    .await
                    .status,
            );
        }

        assert_eq!(statuses.iter().filter(|status| **status == 200).count(), 2);
        assert_eq!(healthy.hits(), 2);
        proxy.finish().await;
    })
    .await
    .expect("ratio budget test timed out");
}

#[tokio::test]
async fn retry_jitter_stretches_each_same_server_retry_delay() {
    timeout(Duration::from_secs(8), async {
        let unavailable = unused_address();
        let healthy = Origin::start("jittered", 1).await;
        let mut route = route(None, "/", &[], "retry");
        let retry = &mut proxy_policy_mut(&mut route).retry;
        retry.target = HttpRetryTarget::SameServer;
        retry.delay_ms = 200;
        retry.jitter_percent = 50;
        retry.final_redispatch = true;
        retry.triggers = vec![HttpRetryTrigger::ConnectFailure];
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[unavailable, healthy.address])],
            vec![route],
            3,
            1,
        )
        .await;

        let (response, elapsed) = timed_get(&proxy, "retry.test").await;

        assert_origin_response(&response, "jittered");
        assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        proxy.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("retry jitter test timed out");
}

#[tokio::test]
async fn a_status_retry_never_returns_to_a_server_that_already_answered() {
    timeout(TEST_TIMEOUT, async {
        let first = StatusOrigin::start(500, "first").await;
        let second = StatusOrigin::start(500, "second").await;
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("retry", &[first.address, second.address])],
            vec![status_retry_route(&[500])],
            3,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: retry.test\r\n")
            .await;

        assert_eq!(response.status, 500);
        assert_eq!((first.hits(), second.hits()), (1, 1));
        proxy.finish().await;
    })
    .await
    .expect("distinct server test timed out");
}
