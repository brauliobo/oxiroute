use crate::TEST_TIMEOUT;
use crate::http_support::HttpResponse as RawResponse;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::Origin;
use crate::support::wire::read_request_head;
use oxiroute_config::DownstreamTimeoutPolicy;
use oxiroute_config::HealthCheck;
use oxiroute_config::HealthCheckType;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::UpstreamPool;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;

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
async fn fail_open_pool_passes_the_apps_own_500_through_when_its_health_path_fails_too() {
    timeout(TEST_TIMEOUT, async {
        let origin = AppErrorOrigin::start().await;
        let proxy = ProxyHarness::start_with_health(
            vec![app_error_pool(origin.address, fail_open())],
            vec![route(None, "/", &[], "app")],
            1,
        )
        .await;
        assert_eq!(proxy.pools[0].health_snapshot().available_endpoints, 0);

        let response = proxy.request("GET / HTTP/1.1\r\nHost: app.test\r\n").await;

        assert_eq!(response.status, 500, "response: {}", response.text());
        assert!(response.text().ends_with("app error"));
        assert_eq!(response.header("x-oxiroute-pool-unavailable"), None);
        let snapshot = proxy.pools[0].health_snapshot();
        assert_eq!(snapshot.fail_open_selections, 1);
        assert_eq!(snapshot.unavailable_selections, 0);
        proxy.finish().await;
    })
    .await
    .expect("fail-open test timed out");
}

#[tokio::test]
async fn pool_without_fail_open_answers_an_empty_503_when_its_health_path_fails() {
    timeout(TEST_TIMEOUT, async {
        let origin = AppErrorOrigin::start().await;
        let proxy = ProxyHarness::start_with_health(
            vec![app_error_pool(
                origin.address,
                oxiroute_config::PoolResilience::default(),
            )],
            vec![route(None, "/", &[], "app")],
            1,
        )
        .await;

        let response = proxy.request("GET / HTTP/1.1\r\nHost: app.test\r\n").await;

        assert_eq!(response.status, 503, "response: {}", response.text());
        assert_eq!(response.header("x-oxiroute-pool-unavailable"), Some("1"));
        assert_eq!(proxy.pools[0].health_snapshot().fail_open_selections, 0);
        proxy.finish().await;
    })
    .await
    .expect("closed pool test timed out");
}

async fn unavailable_response(
    resilience: oxiroute_config::PoolResilience,
    automatic_response_headers: bool,
    method: &str,
) -> RawResponse {
    let origin = AppErrorOrigin::start().await;
    let proxy = ProxyHarness::start_with_features(
        vec![app_error_pool(origin.address, resilience)],
        vec![route(None, "/", &[], "app")],
        Some(1024),
        automatic_response_headers,
        100,
        0,
        true,
        1,
        None,
        None,
        DownstreamTimeoutPolicy::default(),
    )
    .await;
    let response = proxy
        .request(&format!("{method} / HTTP/1.1\r\nHost: app.test\r\n"))
        .await;
    proxy.finish().await;
    response
}

#[tokio::test]
async fn pool_unavailable_503_carries_the_current_date_with_or_without_automatic_headers() {
    timeout(TEST_TIMEOUT, async {
        for automatic in [true, false] {
            let response =
                unavailable_response(oxiroute_config::PoolResilience::default(), automatic, "GET")
                    .await;

            assert_eq!(response.status, 503, "response: {}", response.text());
            assert_eq!(response.header("x-oxiroute-pool-unavailable"), Some("1"));
            let date = httpdate::parse_http_date(response.header("date").expect("Date header"))
                .expect("HTTP date");
            let age = std::time::SystemTime::now()
                .duration_since(date)
                .unwrap_or_default();
            assert!(
                age < Duration::from_mins(1),
                "automatic {automatic}: {age:?}"
            );
            assert_eq!(response.header("retry-after"), None);
            assert_eq!(response.header("content-length"), Some("0"));
        }
    })
    .await
    .expect("503 date test timed out");
}

#[tokio::test]
async fn pool_unavailable_503_adds_the_configured_retry_after_and_body() {
    timeout(TEST_TIMEOUT, async {
        let resilience = oxiroute_config::PoolResilience {
            unavailable_retry_after_seconds: Some(7),
            unavailable_body: Some("temporarily unavailable\n".into()),
            ..oxiroute_config::PoolResilience::default()
        };

        let get = unavailable_response(resilience.clone(), true, "GET").await;
        assert_eq!(get.status, 503, "response: {}", get.text());
        assert_eq!(get.header("retry-after"), Some("7"));
        assert_eq!(
            get.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(get.header("x-oxiroute-pool-unavailable"), Some("1"));
        assert_eq!(get.body(), b"temporarily unavailable\n");

        let head = unavailable_response(resilience, true, "HEAD").await;
        assert_eq!(head.header("content-length"), Some("24"));
        assert!(head.body().is_empty());
    })
    .await
    .expect("503 body test timed out");
}

async fn statuses_while_every_app_server_fails(
    resilience: oxiroute_config::PoolResilience,
) -> Vec<u16> {
    let origins = [AppErrorOrigin::start().await, AppErrorOrigin::start().await];
    let mut app = pool("app", &[origins[0].address, origins[1].address]);
    app.passive_health = Some(oxiroute_config::PassiveHealthPolicy {
        error_limit: 1,
        ..oxiroute_config::PassiveHealthPolicy::default()
    });
    app.resilience = resilience;
    let mut app_route = route(None, "/", &[], "app");
    let HttpRouteAction::Proxy { policy, .. } = &mut app_route.action else {
        panic!("proxy route");
    };
    policy.retry.response_statuses = vec![500];
    let proxy = ProxyHarness::start_with_retries(vec![app], vec![app_route], 1, 3).await;
    let mut statuses = Vec::new();
    for _ in 0..3 {
        statuses.push(
            proxy
                .request("GET / HTTP/1.1\r\nHost: app.test\r\n")
                .await
                .status,
        );
    }
    proxy.finish().await;
    statuses
}

#[tokio::test]
async fn passive_ejection_stops_before_the_last_server_of_a_fail_open_pool() {
    timeout(TEST_TIMEOUT, async {
        let resilience = oxiroute_config::PoolResilience {
            fail_open: true,
            ..oxiroute_config::PoolResilience::default()
        };

        assert_eq!(
            statuses_while_every_app_server_fails(resilience).await,
            [500, 500, 500]
        );
    })
    .await
    .expect("ejection floor test timed out");
}

#[tokio::test]
async fn passive_ejection_without_a_floor_turns_the_apps_500s_into_503s() {
    timeout(TEST_TIMEOUT, async {
        assert_eq!(
            statuses_while_every_app_server_fails(oxiroute_config::PoolResilience::default()).await,
            [500, 503, 503]
        );
    })
    .await
    .expect("ejection without floor test timed out");
}

fn app_error_pool(
    address: SocketAddr,
    resilience: oxiroute_config::PoolResilience,
) -> UpstreamPool {
    UpstreamPool {
        health_check: Some(HealthCheck {
            kind: HealthCheckType::Http,
            interval_ms: 1_000,
            timeout_ms: 500,
            healthy_threshold: 1,
            unhealthy_threshold: 1,
            startup: oxiroute_config::HealthStartup::default(),
            fast_interval_ms: None,
            down_interval_ms: None,
            host: Some("app.test".into()),
            path: Some("/up".into()),
            expected_status: None,
            http_version: None,
        }),
        resilience,
        ..pool("app", &[address])
    }
}

fn fail_open() -> oxiroute_config::PoolResilience {
    oxiroute_config::PoolResilience {
        fail_open: true,
        ..oxiroute_config::PoolResilience::default()
    }
}

/// An origin whose every response, including its health path, is the application's own 500.
struct AppErrorOrigin {
    pub(crate) address: SocketAddr,
    pub(crate) task: JoinHandle<()>,
}

impl AppErrorOrigin {
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                stream
                    .write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 9\r\nConnection: close\r\n\r\napp error",
                    )
                    .await
                    .expect("origin response");
            }
        });
        Self { address, task }
    }
}

impl Drop for AppErrorOrigin {
    fn drop(&mut self) {
        self.task.abort();
    }
}
