//! Wire tests for interactions between pool resilience, route timeouts, rescue and retry settings.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/http.rs"]
mod http_support;

use std::{
    net::SocketAddr,
    os::fd::AsRawFd as _,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, DownstreamTimeoutPolicy, HealthCheck, HealthCheckType,
    HealthStartup, HttpPathSelector, HttpProxyPolicy, HttpProxyTimeouts, HttpRescuePolicy,
    HttpRescueTarget, HttpRetryBudget, HttpRetryTarget, HttpRetryTrigger, HttpRoute,
    HttpRouteAction, HttpService, HttpVersionPolicy, Listener, PassiveHealthPolicy, PoolResilience,
    Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool, UpstreamServer,
};
use oxiroute_server::{
    HttpDownstreamPolicyApp, HttpReverseProxy, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RoundRobinPool,
    RuntimeGeneration, RuntimeMetrics, ServiceKind,
};
use pingora::{
    apps::ServerApp,
    protocols::{GetSocketDigest as _, SocketDigest},
    proxy::http_proxy,
    server::configuration::ServerConf,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::{HttpResponse, read_request_head};

const TEST_TIMEOUT: Duration = Duration::from_secs(8);

struct Proxy {
    address: SocketAddr,
    generation: Arc<RuntimeGeneration>,
    pools: Vec<Arc<RoundRobinPool>>,
    _shutdown_tx: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl Proxy {
    #[allow(clippy::too_many_lines)]
    /// A proxy that accepts `connections` client connections; the service deadline is one second.
    async fn start(
        upstream_pools: Vec<UpstreamPool>,
        mut routes: Vec<HttpRoute>,
        max_retries: u8,
        connections: usize,
    ) -> Self {
        for route in &mut routes {
            policy_mut(route).retry.max_retries = max_retries;
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let address = listener.local_addr().expect("proxy address");
        drop(listener);
        let config = ConfigDraft {
            listeners: vec![Listener {
                name: "http-loopback".into(),
                bind: socket_bind(address),
                protocol: Protocol::Http,
                service: Some("routing".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: Some(100),
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            }],
            upstream_pools,
            http_services: vec![HttpService {
                name: "routing".into(),
                routes,
                automatic_response_headers: true,
                upstream_io_timeout_ms: 1_000,
                max_request_body_bytes: Some(1024),
                gzip: None,
                access_log: None,
            }],
            ..empty_config()
        };
        let config = config.validate().expect("valid canonical HTTP config");
        let generation = config_support::runtime_generation(&config).expect("runtime generation");
        let listener = std::net::TcpListener::from(
            generation
                .reservations()
                .get("http-loopback")
                .expect("HTTP listener reservation")
                .duplicate_owned_fd()
                .expect("duplicate HTTP listener"),
        );
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let listener = TcpListener::from_std(listener).expect("Tokio HTTP listener");
        let pools = generation.pools().to_vec();
        let spec = generation.services().to_vec().remove(0);
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
        let configuration = Arc::new(ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        });
        let app = Arc::new(MonitoredHttpApp::new(
            HttpDownstreamPolicyApp::new(
                http_proxy(
                    &configuration,
                    HttpReverseProxy::new(service, listener_metrics.clone())
                        .with_generation(Arc::clone(&generation)),
                ),
                downstream_timeouts,
            ),
            listener_metrics,
            Arc::clone(&generation),
        ));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut tasks = Vec::with_capacity(connections);
            for _ in 0..connections {
                let (stream, _) = listener.accept().await.expect("proxy accept");
                let Some(admission) = app.admit_connection() else {
                    continue;
                };
                let mut stream = pingora::protocols::l4::stream::Stream::from(stream);
                stream.set_socket_digest(SocketDigest::from_raw_fd(stream.as_raw_fd()));
                let stream: pingora::protocols::Stream = Box::new(stream);
                let app = Arc::clone(&app);
                let shutdown = shutdown.clone();
                tasks.push(tokio::spawn(async move {
                    let _admission = admission;
                    app.process_new(stream, &shutdown).await;
                }));
            }
            for task in tasks {
                task.await.expect("proxy connection task");
            }
        });
        Self {
            address,
            generation,
            pools,
            _shutdown_tx: shutdown_tx,
            task: Some(task),
        }
    }

    async fn request(&self, method: &str) -> HttpResponse {
        let request = format!("{method} / HTTP/1.1\r\nHost: app.test\r\nConnection: close\r\n\r\n");
        http_support::raw_http_request(self.address, request.as_bytes()).await
    }

    async fn timed(&self, method: &str) -> (HttpResponse, Duration) {
        let started = Instant::now();
        let response = self.request(method).await;
        (response, started.elapsed())
    }

    async fn finish(mut self) {
        self.task
            .take()
            .expect("proxy task")
            .await
            .expect("proxy task completed");
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// An origin answering every request, including health probes, with `status` and its own name.
struct Origin {
    address: SocketAddr,
    hits: Arc<Mutex<usize>>,
    task: JoinHandle<()>,
}

impl Origin {
    async fn start(status: u16, name: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let hits = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&hits);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                *counted.lock().expect("hit counter") += 1;
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nX-Origin: {name}\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        Self {
            address,
            hits,
            task,
        }
    }

    /// An address nothing listens on.
    async fn closed_address() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        listener.local_addr().expect("address")
    }

    fn hits(&self) -> usize {
        *self.hits.lock().expect("hit counter")
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// An origin that accepts a connection and never answers.
async fn silent_origin() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().expect("origin address");
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            tokio::spawn(async move {
                let mut sink = Vec::new();
                let _ = stream.read_to_end(&mut sink).await;
            });
        }
    });
    (address, task)
}

fn server(name: &str, address: SocketAddr) -> UpstreamServer {
    UpstreamServer {
        name: name.into(),
        endpoint: socket_endpoint(address),
        max_connections: None,
        dns_resolution: DnsResolutionPolicy::OnConnect,
    }
}

fn pool(name: &str, algorithm: UpstreamAlgorithm, servers: Vec<UpstreamServer>) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers,
        endpoints: Vec::new(),
        algorithm,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::default(),
        registration: None,
        resilience: PoolResilience::default(),
    }
}

/// A one-server pool that has never been probed, so no server is selectable until `probe_once`.
fn unprobed_pool(address: SocketAddr, resilience: PoolResilience) -> UpstreamPool {
    UpstreamPool {
        health_check: Some(HealthCheck {
            kind: HealthCheckType::Http,
            interval_ms: 1_000,
            timeout_ms: 500,
            healthy_threshold: 1,
            unhealthy_threshold: 1,
            startup: HealthStartup::Checking,
            fast_interval_ms: None,
            down_interval_ms: None,
            host: Some("app.test".into()),
            path: Some("/up".into()),
            expected_status: None,
            http_version: None,
        }),
        resilience,
        ..pool(
            "app",
            UpstreamAlgorithm::First,
            vec![server("only", address)],
        )
    }
}

fn passive_limit_one() -> PassiveHealthPolicy {
    PassiveHealthPolicy {
        error_limit: 1,
        ..PassiveHealthPolicy::default()
    }
}

fn route(pool: &str) -> HttpRoute {
    HttpRoute {
        host: None,
        path: HttpPathSelector::SegmentPrefix { value: "/".into() },
        methods: Vec::new(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::Proxy {
            upstream_pool: pool.into(),
            policy: HttpProxyPolicy::default(),
        },
    }
}

fn policy_mut(route: &mut HttpRoute) -> &mut HttpProxyPolicy {
    let HttpRouteAction::Proxy { policy, .. } = &mut route.action else {
        panic!("proxy route");
    };
    policy
}

fn total_route(total_ms: u64) -> HttpRoute {
    let mut route = route("app");
    policy_mut(&mut route).timeouts = HttpProxyTimeouts {
        total_ms: Some(total_ms),
        ..HttpProxyTimeouts::default()
    };
    route
}

fn rescue_route(pool: &str, target: HttpRescueTarget, rescue_pool: Option<&str>) -> HttpRoute {
    let mut route = route(pool);
    policy_mut(&mut route).rescue = Some(HttpRescuePolicy {
        statuses: vec![404],
        target,
        pool: rescue_pool.map(str::to_owned),
    });
    route
}

fn status_retry_route(pool: &str, statuses: &[u16]) -> HttpRoute {
    let mut route = route(pool);
    policy_mut(&mut route).retry.response_statuses = statuses.to_vec();
    route
}

fn assert_valid_date(response: &HttpResponse) {
    let date = httpdate::parse_http_date(response.header("date").expect("Date header"))
        .expect("HTTP date");
    let age = SystemTime::now().duration_since(date).unwrap_or_default();
    assert!(age < Duration::from_secs(60), "{age:?}");
}

fn assert_not_a_pool_refusal(response: &HttpResponse) {
    assert_eq!(response.header("retry-after"), None);
    assert_eq!(response.header("x-oxiroute-pool-unavailable"), None);
}

fn unavailable_resilience(wait_ms: Option<u64>) -> PoolResilience {
    PoolResilience {
        queue_while_unavailable_ms: wait_ms,
        unavailable_retry_after_seconds: Some(7),
        unavailable_body: Some("temporarily unavailable\n".into()),
        ..PoolResilience::default()
    }
}

// 1. queue_while_unavailable_ms against the route deadline.

#[tokio::test]
async fn the_unavailable_queue_wait_is_clamped_by_the_route_total() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start(200, "up").await;
        let proxy = Proxy::start(
            vec![unprobed_pool(
                origin.address,
                unavailable_resilience(Some(3_000)),
            )],
            vec![total_route(200)],
            0,
            1,
        )
        .await;

        let (response, elapsed) = proxy.timed("GET").await;

        assert_eq!(response.status, 504, "response: {}", response.text());
        assert_eq!(response.header("x-oxiroute-reason"), Some("total_deadline"));
        assert_not_a_pool_refusal(&response);
        assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(700), "{elapsed:?}");
        assert_eq!(origin.hits(), 0);
        proxy.finish().await;
    })
    .await
    .expect("route total clamp test timed out");
}

#[tokio::test]
async fn the_unavailable_queue_wait_is_clamped_by_the_service_deadline() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start(200, "up").await;
        let proxy = Proxy::start(
            vec![unprobed_pool(
                origin.address,
                unavailable_resilience(Some(5_000)),
            )],
            vec![route("app")],
            0,
            1,
        )
        .await;

        let (response, elapsed) = proxy.timed("GET").await;

        assert_eq!(response.status, 504, "response: {}", response.text());
        assert_eq!(response.header("x-oxiroute-reason"), Some("total_deadline"));
        assert!(elapsed < Duration::from_millis(1_700), "{elapsed:?}");
        proxy.finish().await;
    })
    .await
    .expect("service deadline clamp test timed out");
}

#[tokio::test]
async fn a_queue_wait_shorter_than_the_route_total_ends_in_the_pool_503() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start(200, "up").await;
        let proxy = Proxy::start(
            vec![unprobed_pool(
                origin.address,
                unavailable_resilience(Some(300)),
            )],
            vec![total_route(900)],
            0,
            1,
        )
        .await;

        let (response, elapsed) = proxy.timed("GET").await;

        assert_eq!(response.status, 503, "response: {}", response.text());
        assert_eq!(response.header("x-oxiroute-pool-unavailable"), Some("1"));
        assert_eq!(response.header("retry-after"), Some("7"));
        assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(800), "{elapsed:?}");
        proxy.finish().await;
    })
    .await
    .expect("short queue wait test timed out");
}

#[tokio::test]
async fn a_server_recovering_inside_the_route_total_serves_the_queued_request() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start(200, "recovered").await;
        let proxy = Proxy::start(
            vec![unprobed_pool(
                origin.address,
                unavailable_resilience(Some(3_000)),
            )],
            vec![total_route(900)],
            0,
            1,
        )
        .await;
        let generation = Arc::clone(&proxy.generation);
        tokio::spawn(async move {
            sleep(Duration::from_millis(150)).await;
            generation
                .health_supervisor()
                .expect("health supervisor")
                .probe_once()
                .await;
        });

        let (response, elapsed) = proxy.timed("GET").await;

        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.body(), b"recovered");
        assert!(elapsed < Duration::from_millis(800), "{elapsed:?}");
        proxy.finish().await;
    })
    .await
    .expect("recovery inside total test timed out");
}

// 2. Rescue to a named pool with server names shared by both pools.

fn two_named(pool_name: &str, first: &Origin, second: &Origin) -> UpstreamPool {
    pool(
        pool_name,
        UpstreamAlgorithm::First,
        vec![server("one", first.address), server("two", second.address)],
    )
}

#[tokio::test]
async fn a_rescue_pool_may_reuse_the_server_names_of_the_pool_that_failed() {
    timeout(TEST_TIMEOUT, async {
        let (primary_one, primary_two) = (
            Origin::start(500, "p1").await,
            Origin::start(404, "p2").await,
        );
        let (spare_one, spare_two) = (
            Origin::start(200, "s1").await,
            Origin::start(200, "s2").await,
        );
        let mut route = rescue_route("primary", HttpRescueTarget::Pool, Some("spare"));
        policy_mut(&mut route).retry.response_statuses = vec![500];
        let proxy = Proxy::start(
            vec![
                two_named("primary", &primary_one, &primary_two),
                two_named("spare", &spare_one, &spare_two),
            ],
            vec![route],
            3,
            1,
        )
        .await;

        let response = proxy.request("GET").await;

        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(
            response.body(),
            b"s1",
            "spare server `one` is a different server"
        );
        assert_eq!((primary_one.hits(), primary_two.hits()), (1, 1));
        assert_eq!((spare_one.hits(), spare_two.hits()), (1, 0));
        proxy.finish().await;
    })
    .await
    .expect("shared server names test timed out");
}

#[tokio::test]
async fn the_rescue_pool_tries_each_of_its_servers_once_even_when_they_all_answer_the_rescue_status()
 {
    timeout(TEST_TIMEOUT, async {
        let primary = Origin::start(404, "primary").await;
        let (spare_one, spare_two) = (
            Origin::start(404, "s1").await,
            Origin::start(404, "s2").await,
        );
        let proxy = Proxy::start(
            vec![
                pool(
                    "primary",
                    UpstreamAlgorithm::First,
                    vec![server("one", primary.address)],
                ),
                two_named("spare", &spare_one, &spare_two),
            ],
            vec![rescue_route(
                "primary",
                HttpRescueTarget::Pool,
                Some("spare"),
            )],
            3,
            1,
        )
        .await;

        let response = proxy.request("GET").await;

        assert_eq!(response.status, 404, "response: {}", response.text());
        assert_eq!(response.header("x-origin"), Some("s2"));
        assert_eq!(
            (primary.hits(), spare_one.hits(), spare_two.hits()),
            (1, 1, 1),
            "no server is revisited"
        );
        proxy.finish().await;
    })
    .await
    .expect("rescue pool exhaustion test timed out");
}

// 3. A rescue status is not a passive-health failure.

fn resilient_pool(name: &str, first: &Origin, second: &Origin) -> UpstreamPool {
    UpstreamPool {
        passive_health: Some(passive_limit_one()),
        resilience: PoolResilience {
            fail_open: true,
            min_members: Some(1),
            ..PoolResilience::default()
        },
        ..two_named(name, first, second)
    }
}

fn available_servers(proxy: &Proxy) -> usize {
    proxy.pools[0].health_snapshot().available_endpoints
}

#[tokio::test]
async fn a_rescued_status_never_ejects_the_server_that_answered_it() {
    timeout(TEST_TIMEOUT, async {
        let (missing, healthy) = (
            Origin::start(404, "missing").await,
            Origin::start(200, "healthy").await,
        );
        let proxy = Proxy::start(
            vec![resilient_pool("app", &missing, &healthy)],
            vec![rescue_route("app", HttpRescueTarget::NextServer, None)],
            1,
            3,
        )
        .await;

        for _ in 0..3 {
            let response = proxy.request("GET").await;
            assert_eq!(response.status, 200, "response: {}", response.text());
            assert_eq!(response.body(), b"healthy");
        }

        assert_eq!(missing.hits(), 3, "the first server stays in rotation");
        assert_eq!(available_servers(&proxy), 2);
        proxy.finish().await;
    })
    .await
    .expect("rescue passive health test timed out");
}

#[tokio::test]
async fn a_retry_status_ejects_where_a_rescue_status_does_not() {
    timeout(TEST_TIMEOUT, async {
        let (failing, healthy) = (
            Origin::start(500, "failing").await,
            Origin::start(200, "healthy").await,
        );
        let proxy = Proxy::start(
            vec![resilient_pool("app", &failing, &healthy)],
            vec![status_retry_route("app", &[500])],
            1,
            1,
        )
        .await;

        assert_eq!(proxy.request("GET").await.status, 200);

        assert_eq!(available_servers(&proxy), 1);
        proxy.finish().await;
    })
    .await
    .expect("retry status control test timed out");
}

#[tokio::test]
async fn a_lone_server_answering_the_rescue_status_returns_it_unchanged_and_stays_selectable() {
    timeout(TEST_TIMEOUT, async {
        let only = Origin::start(404, "only").await;
        let proxy = Proxy::start(
            vec![UpstreamPool {
                passive_health: Some(passive_limit_one()),
                resilience: PoolResilience {
                    fail_open: true,
                    min_members: Some(1),
                    ..PoolResilience::default()
                },
                ..pool(
                    "app",
                    UpstreamAlgorithm::First,
                    vec![server("only", only.address)],
                )
            }],
            vec![rescue_route("app", HttpRescueTarget::NextServer, None)],
            3,
            3,
        )
        .await;

        for _ in 0..3 {
            let response = proxy.request("GET").await;
            assert_eq!(response.status, 404, "response: {}", response.text());
            assert_eq!(response.header("x-origin"), Some("only"));
            assert_eq!(response.body(), b"only");
        }

        assert_eq!(only.hits(), 3, "one attempt per request, no replay");
        assert_eq!(available_servers(&proxy), 1);
        assert_eq!(proxy.pools[0].health_snapshot().fail_open_selections, 0);
        proxy.finish().await;
    })
    .await
    .expect("lone rescue server test timed out");
}

// 4. The pool-unavailable 503 on a route with timeouts.

#[tokio::test]
async fn the_pool_503_keeps_its_body_retry_after_and_date_on_a_route_with_timeouts() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start(200, "up").await;
        let mut route = total_route(800);
        policy_mut(&mut route).timeouts.header_ms = Some(300);
        let proxy = Proxy::start(
            vec![unprobed_pool(origin.address, unavailable_resilience(None))],
            vec![route],
            0,
            2,
        )
        .await;

        let get = proxy.request("GET").await;
        assert_eq!(get.status, 503, "response: {}", get.text());
        assert_eq!(get.header("x-oxiroute-pool-unavailable"), Some("1"));
        assert_eq!(get.header("retry-after"), Some("7"));
        assert_eq!(get.header("x-oxiroute-reason"), None);
        assert_eq!(get.body(), b"temporarily unavailable\n");
        assert_valid_date(&get);

        let head = proxy.request("HEAD").await;
        assert_eq!(head.status, 503);
        assert_eq!(head.header("retry-after"), Some("7"));
        assert_eq!(head.header("content-length"), Some("24"));
        assert_eq!(head.body(), b"");
        assert_valid_date(&head);
        proxy.finish().await;
    })
    .await
    .expect("pool 503 with timeouts test timed out");
}

#[tokio::test]
async fn a_504_never_carries_retry_after_or_the_unavailable_body() {
    timeout(TEST_TIMEOUT, async {
        let (silent, _task) = silent_origin().await;
        let mut route = route("app");
        policy_mut(&mut route).timeouts.header_ms = Some(150);
        let mut healthy_pool = pool(
            "app",
            UpstreamAlgorithm::First,
            vec![server("only", silent)],
        );
        healthy_pool.resilience = unavailable_resilience(None);
        let proxy = Proxy::start(vec![healthy_pool], vec![route], 0, 2).await;

        for method in ["GET", "HEAD"] {
            let response = proxy.request(method).await;
            assert_eq!(response.status, 504, "response: {}", response.text());
            assert_eq!(
                response.header("x-oxiroute-reason"),
                Some("upstream_header_timeout")
            );
            assert_not_a_pool_refusal(&response);
            assert!(!response.text().contains("temporarily unavailable"));
            assert_valid_date(&response);
        }
        proxy.finish().await;
    })
    .await
    .expect("504 headers test timed out");
}

// 5. Retry budget and jitter with a pool that fails open.

fn jittered(route: &mut HttpRoute, target: HttpRetryTarget, per_second: u32) {
    let retry = &mut policy_mut(route).retry;
    retry.target = target;
    retry.delay_ms = 100;
    retry.jitter_percent = 50;
    retry.budget = Some(HttpRetryBudget {
        per_second: Some(per_second),
        ratio_percent: None,
    });
}

fn lone_fail_open_pool(address: SocketAddr) -> UpstreamPool {
    UpstreamPool {
        passive_health: Some(passive_limit_one()),
        resilience: PoolResilience {
            fail_open: true,
            ..PoolResilience::default()
        },
        ..pool(
            "app",
            UpstreamAlgorithm::First,
            vec![server("only", address)],
        )
    }
}

#[tokio::test]
async fn with_no_other_server_left_a_status_retry_returns_the_backend_response_at_once() {
    timeout(TEST_TIMEOUT, async {
        let only = Origin::start(500, "only").await;
        let mut route = status_retry_route("app", &[500]);
        jittered(&mut route, HttpRetryTarget::NextServer, 1);
        let proxy = Proxy::start(vec![lone_fail_open_pool(only.address)], vec![route], 3, 3).await;

        for _ in 0..3 {
            let (response, elapsed) = proxy.timed("GET").await;
            assert_eq!(response.status, 500, "response: {}", response.text());
            assert_eq!(response.body(), b"only");
            assert!(
                elapsed < Duration::from_millis(100),
                "no retry delay: {elapsed:?}"
            );
        }

        assert_eq!(only.hits(), 3, "no retry storm");
        proxy.finish().await;
    })
    .await
    .expect("lone server retry test timed out");
}

#[tokio::test]
async fn a_same_server_status_retry_stops_when_the_budget_is_spent() {
    timeout(TEST_TIMEOUT, async {
        let only = Origin::start(500, "only").await;
        let mut route = status_retry_route("app", &[500]);
        jittered(&mut route, HttpRetryTarget::SameServer, 2);
        let proxy = Proxy::start(vec![lone_fail_open_pool(only.address)], vec![route], 3, 2).await;

        let (response, elapsed) = proxy.timed("GET").await;
        assert_eq!(response.status, 500, "response: {}", response.text());
        assert_eq!(response.body(), b"only");
        assert_eq!(
            only.hits(),
            3,
            "the original try and the two budgeted retries"
        );
        assert!(
            elapsed >= Duration::from_millis(190),
            "two jittered delays: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_millis(900), "{elapsed:?}");

        let (response, elapsed) = proxy.timed("GET").await;
        assert_eq!(response.status, 500, "response: {}", response.text());
        assert_eq!(
            only.hits(),
            4,
            "the budget of this second is spent: no retry"
        );
        assert!(elapsed < Duration::from_millis(100), "{elapsed:?}");
        proxy.finish().await;
    })
    .await
    .expect("same server budget test timed out");
}

#[tokio::test]
async fn a_connect_failure_retry_on_a_lone_server_stops_when_the_budget_is_spent() {
    timeout(TEST_TIMEOUT, async {
        let mut route = route("app");
        jittered(&mut route, HttpRetryTarget::SameServer, 1);
        policy_mut(&mut route).retry.triggers = vec![HttpRetryTrigger::ConnectFailure];
        let proxy = Proxy::start(
            vec![lone_fail_open_pool(Origin::closed_address().await)],
            vec![route],
            3,
            1,
        )
        .await;

        let (response, elapsed) = proxy.timed("GET").await;

        assert_eq!(response.status, 502, "response: {}", response.text());
        assert!(
            elapsed >= Duration::from_millis(90),
            "one retry delay: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_millis(700), "{elapsed:?}");
        proxy.finish().await;
    })
    .await
    .expect("connect failure budget test timed out");
}

#[tokio::test]
async fn an_exhausted_budget_delivers_the_rescued_status_instead_of_replaying() {
    timeout(TEST_TIMEOUT, async {
        let (missing, healthy) = (
            Origin::start(404, "missing").await,
            Origin::start(200, "healthy").await,
        );
        let mut route = rescue_route("app", HttpRescueTarget::NextServer, None);
        jittered(&mut route, HttpRetryTarget::NextServer, 1);
        let proxy = Proxy::start(
            vec![two_named("app", &missing, &healthy)],
            vec![route],
            1,
            2,
        )
        .await;

        let first = proxy.request("GET").await;
        assert_eq!((first.status, first.body()), (200, b"healthy".as_slice()));

        let second = proxy.request("GET").await;
        assert_eq!((second.status, second.body()), (404, b"missing".as_slice()));
        assert_eq!(healthy.hits(), 1);
        proxy.finish().await;
    })
    .await
    .expect("rescue budget test timed out");
}
