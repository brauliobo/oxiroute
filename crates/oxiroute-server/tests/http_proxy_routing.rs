use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use oxiroute_config::{
    Config, HttpRoute, HttpService, Listener, Protocol, UpstreamAlgorithm, UpstreamPool,
};
use oxiroute_server::{
    HttpReverseProxy, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RuntimeMetrics, ServiceKind,
    service_specs,
};
use pingora::{apps::ServerApp, proxy::http_proxy, server::configuration::ServerConf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
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

struct ProxyHarness {
    address: SocketAddr,
    metrics: RuntimeMetrics,
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
            max_request_body_bytes,
            100,
            0,
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
            max_request_body_bytes,
            max_connections,
            0,
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
            1024,
            100,
            max_retries,
            expected_connections,
        )
        .await
    }

    async fn start_with_policy(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        max_request_body_bytes: u64,
        max_connections: u64,
        max_retries: u8,
        expected_connections: usize,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let address = listener.local_addr().expect("proxy address");
        let config = Config {
            version: 1,
            management: None,
            listeners: vec![Listener {
                name: "http-loopback".into(),
                bind: address,
                protocol: Protocol::Http,
                service: Some("routing".into()),
                max_connections,
            }],
            upstream_pools,
            http_services: vec![HttpService {
                name: "routing".into(),
                routes,
                upstream_io_timeout_ms: 1_000,
                max_request_body_bytes,
                max_retries,
            }],
            l4_services: Vec::new(),
        };
        let mut specs = service_specs(&config).expect("canonical HTTP service plan");
        let spec = specs.remove(0);
        let metrics = RuntimeMetrics::new();
        let listener_metrics = metrics
            .register_listener(
                &spec.name,
                spec.kind.protocol(),
                spec.bind.to_string(),
                spec.max_connections,
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
                let stream: pingora::protocols::Stream =
                    Box::new(pingora::protocols::l4::stream::Stream::from(stream));
                let proxy = Arc::clone(&proxy);
                let shutdown = shutdown.clone();
                connections.push(tokio::spawn(async move {
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
            _shutdown_tx: shutdown_tx,
            task: Some(task),
        }
    }

    async fn request(&self, request_head: &str) -> RawResponse {
        self.request_bytes(format!("{request_head}Connection: close\r\n\r\n").as_bytes())
            .await
    }

    async fn request_bytes(&self, request: &[u8]) -> RawResponse {
        let mut stream = TcpStream::connect(self.address)
            .await
            .expect("client connect");
        stream
            .write_all(request)
            .await
            .expect("client request write");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("client response read");
        RawResponse::parse(response)
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

struct RawResponse {
    status: u16,
    bytes: Vec<u8>,
}

impl RawResponse {
    fn parse(bytes: Vec<u8>) -> Self {
        let status = bytes
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .and_then(|line| line.split_ascii_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .unwrap_or_else(|| {
                panic!("invalid HTTP response: {}", String::from_utf8_lossy(&bytes))
            });
        Self { status, bytes }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

async fn read_request_head(stream: &mut TcpStream) -> io::Result<()> {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "origin request ended before its headers",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
    }
    Ok(())
}

fn pool(name: &str, endpoints: &[SocketAddr]) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        endpoints: endpoints.to_vec(),
        algorithm: UpstreamAlgorithm::RoundRobin,
    }
}

fn route(host: Option<&str>, path: &str, methods: &[&str], pool: &str) -> HttpRoute {
    HttpRoute {
        host: host.map(str::to_owned),
        path_prefix: path.into(),
        methods: methods.iter().map(ToString::to_string).collect(),
        upstream_pool: pool.into(),
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
