use crate::config_support;
use crate::config_support::empty_config;
use crate::config_support::socket_bind;
use crate::http_support::HttpResponse as RawResponse;
use crate::http_support::raw_http_request;
use crate::ports_support;
use crate::support::builders::memory_cache_store;
use oxiroute_config::AccessLogPolicy;
use oxiroute_config::CacheStore;
use oxiroute_config::ConfigDraft;
use oxiroute_config::DownstreamTimeoutPolicy;
use oxiroute_config::HttpGzipPolicy;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::HttpService;
use oxiroute_config::Listener;
use oxiroute_config::Protocol;
use oxiroute_config::UpstreamPool;
use oxiroute_server::HttpDownstreamPolicyApp;
use oxiroute_server::HttpReverseProxy;
use oxiroute_server::MAX_HTTP_ATTEMPTS;
use oxiroute_server::MonitoredHttpApp;
use oxiroute_server::RoundRobinPool;
use oxiroute_server::RuntimeMetrics;
use oxiroute_server::ServiceKind;
use pingora::apps::ServerApp;
use pingora::protocols::GetSocketDigest as _;
use pingora::protocols::SocketDigest;
use pingora::proxy::http_proxy;
use pingora::server::configuration::ServerConf;
use std::net::SocketAddr;
use std::os::fd::AsRawFd as _;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::net::TcpSocket;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::timeout;

pub(crate) struct ProxyHarness {
    pub(crate) address: SocketAddr,
    _generation: Arc<oxiroute_server::RuntimeGeneration>,
    pub(crate) metrics: RuntimeMetrics,
    pub(crate) pools: Vec<Arc<RoundRobinPool>>,
    _shutdown_tx: watch::Sender<bool>,
    pub(crate) task: Option<JoinHandle<()>>,
}

impl ProxyHarness {
    pub(crate) async fn start(
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

    pub(crate) async fn start_with_memory_cache(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        expected_connections: usize,
    ) -> Self {
        Self::start_with_features_and_cache(
            upstream_pools,
            routes,
            Some(1024),
            true,
            100,
            0,
            false,
            expected_connections,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
            vec![memory_cache_store("memory")],
        )
        .await
    }

    pub(crate) async fn start_with_limit(
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

    pub(crate) async fn start_unbounded(
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

    pub(crate) async fn start_with_retries(
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

    pub(crate) async fn start_with_health(
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
            true,
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
    pub(crate) async fn start_with_features(
        upstream_pools: Vec<UpstreamPool>,
        routes: Vec<HttpRoute>,
        max_request_body_bytes: Option<u64>,
        automatic_response_headers: bool,
        max_connections: u64,
        max_retries: u8,
        run_health_checks: bool,
        expected_connections: usize,
        gzip: Option<HttpGzipPolicy>,
        access_log: Option<AccessLogPolicy>,
        downstream_timeouts: DownstreamTimeoutPolicy,
    ) -> Self {
        Self::start_with_features_and_cache(
            upstream_pools,
            routes,
            max_request_body_bytes,
            automatic_response_headers,
            max_connections,
            max_retries,
            run_health_checks,
            expected_connections,
            gzip,
            access_log,
            downstream_timeouts,
            Vec::new(),
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "wire harness exposes canonical service policy"
    )]
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn start_with_features_and_cache(
        upstream_pools: Vec<UpstreamPool>,
        mut routes: Vec<HttpRoute>,
        max_request_body_bytes: Option<u64>,
        automatic_response_headers: bool,
        max_connections: u64,
        max_retries: u8,
        run_health_checks: bool,
        expected_connections: usize,
        gzip: Option<HttpGzipPolicy>,
        access_log: Option<AccessLogPolicy>,
        downstream_timeouts: DownstreamTimeoutPolicy,
        cache_stores: Vec<CacheStore>,
    ) -> Self {
        for route in &mut routes {
            if let HttpRouteAction::Proxy { policy, .. } = &mut route.action {
                policy.retry.max_retries = max_retries;
            }
        }
        let address = ports_support::unused_address();
        let config = ConfigDraft {
            listener_workers: None,
            http_fast_lane: None,
            connection_scheduling: None,
            connection_age_ms: None,
            worker_scaling: None,
            min_active_workers: None,
            target_busy_percent: None,
            scale_down_delay_ms: None,
            tls_handshake_threads: None,
            http3_congestion_control: None,
            http3_segmentation_offload: None,
            retirement: None,
            listeners: vec![Listener {
                name: "http-loopback".into(),
                bind: socket_bind(address),
                protocol: Protocol::Http,
                service: Some("routing".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: Some(max_connections),
                downstream_timeouts,
                h2_initial_stream_window: None,
                h2_initial_connection_window: None,
            }],
            upstream_pools,
            http_services: vec![HttpService {
                drain_deadline_ms: None,
                name: "routing".into(),
                routes,
                automatic_response_headers,
                upstream_io_timeout_ms: 1_000,
                max_request_body_bytes,
                gzip,
                access_log,
            }],
            cache_stores,
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
            .expect("nonblocking HTTP listener");
        let listener = TcpListener::from_std(listener).expect("Tokio HTTP listener");
        let pools = generation.pools().to_vec();
        if run_health_checks {
            generation
                .health_supervisor()
                .as_ref()
                .expect("health supervisor")
                .probe_once()
                .await;
        }
        let mut specs = generation.services().to_vec();
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
            _generation: generation,
            metrics,
            pools,
            _shutdown_tx: shutdown_tx,
            task: Some(task),
        }
    }

    pub(crate) async fn request(&self, request_head: &str) -> RawResponse {
        self.request_bytes(format!("{request_head}Connection: close\r\n\r\n").as_bytes())
            .await
    }

    pub(crate) async fn request_bytes(&self, request: &[u8]) -> RawResponse {
        raw_http_request(self.address, request).await
    }

    pub(crate) async fn finish(mut self) {
        let task = self.task.take().expect("proxy task");
        task.await.expect("proxy task completed");
    }

    pub(crate) async fn wait_for_active_connections(&self, expected: u64) {
        timeout(Duration::from_secs(30), async {
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

    pub(crate) async fn wait_for_no_active_leases(&self) {
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

/// Binds a loopback socket that does not listen: connections to it are refused, and no other test
/// of this process can be handed its port by `bind(:0)` while the socket stays bound.
pub(crate) fn unlistened_socket() -> TcpSocket {
    let socket = TcpSocket::new_v4().expect("unlistened socket");
    socket
        .bind("127.0.0.1:0".parse().expect("loopback address"))
        .expect("unlistened socket bind");
    socket
}

pub(crate) fn unused_address() -> SocketAddr {
    unused_addresses(1)[0]
}

pub(crate) fn unused_addresses(count: usize) -> Vec<SocketAddr> {
    static HELD: Mutex<Vec<TcpSocket>> = Mutex::new(Vec::new());
    let sockets: Vec<_> = (0..count).map(|_| unlistened_socket()).collect();
    let addresses = sockets
        .iter()
        .map(|socket| socket.local_addr().expect("unused address"))
        .collect();
    HELD.lock().unwrap().extend(sockets);
    addresses
}

impl Drop for ProxyHarness {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
