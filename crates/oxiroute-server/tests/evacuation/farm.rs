use super::*;

/// How the pool, the route and the origins are set up.
pub struct Layout {
    pub pool: &'static str,
    pub modes: Vec<Mode>,
    pub capacity: Option<u64>,
    pub queue_ms: Option<u64>,
    pub replayable_posts: bool,
    pub access_log: bool,
}

impl Layout {
    /// A pool named after the test, so that tests of one process never evacuate each other's requests.
    pub fn new(pool: &'static str, modes: &[Mode]) -> Self {
        Self {
            pool,
            modes: modes.to_vec(),
            capacity: None,
            queue_ms: None,
            replayable_posts: false,
            access_log: false,
        }
    }

    pub fn capacity(mut self, capacity: u64) -> Self {
        self.capacity = Some(capacity);
        self
    }

    pub fn queue(mut self) -> Self {
        self.queue_ms = Some(30_000);
        self
    }

    pub fn replayable_posts(mut self) -> Self {
        self.replayable_posts = true;
        self
    }

    pub fn access_log(mut self) -> Self {
        self.access_log = true;
        self
    }

    /// No retries at all: evacuation must work with `max_retries = 0` and spend no retry budget.
    pub fn retry(&self) -> HttpRetryPolicy {
        let posts = self.replayable_posts;
        HttpRetryPolicy {
            max_retries: 0,
            method_safety: if posts {
                HttpRetryMethodSafety::All
            } else {
                HttpRetryMethodSafety::GetHead
            },
            body_safety: if posts {
                HttpRetryBodySafety::Buffered
            } else {
                HttpRetryBodySafety::Empty
            },
            idempotency_header: posts.then(|| "idempotency-key".to_owned()),
            budget: Some(HttpRetryBudget {
                per_second: Some(1),
                ratio_percent: None,
            }),
            ..HttpRetryPolicy::default()
        }
    }
}

pub fn server_name(index: usize) -> String {
    format!(
        "node-{}",
        char::from(b'a' + u8::try_from(index).expect("few servers"))
    )
}

pub struct Farm {
    pub address: SocketAddr,
    pub pool: Arc<RoundRobinPool>,
    pub origins: Vec<Origin>,
    pub access_log: PathBuf,
    pub _directory: TempDir,
    pub _generation: Arc<oxiroute_server::RuntimeGeneration>,
    pub _shutdown: watch::Sender<bool>,
    pub task: JoinHandle<()>,
}

impl Farm {
    pub async fn start(layout: Layout) -> Self {
        let mut origins = Vec::new();
        for (index, mode) in layout.modes.iter().enumerate() {
            origins.push(Origin::start(server_name(index), *mode).await);
        }
        let address = ports_support::unused_address();
        let directory = TempDir::new().expect("access log directory");
        let access_log = directory.path().join("access.jsonl");
        let config = config(
            &layout,
            address,
            &origins,
            layout.access_log.then(|| access_log.clone()),
        );
        let config = config.validate().expect("valid evacuation config");
        let generation = config_support::runtime_generation(&config).expect("runtime generation");
        let listener = std::net::TcpListener::from(
            generation
                .reservations()
                .get("http-loopback")
                .expect("listener reservation")
                .duplicate_owned_fd()
                .expect("duplicate listener"),
        );
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        let pool = Arc::clone(&generation.pools()[0]);
        let spec = generation.services().to_vec().remove(0);
        let metrics = RuntimeMetrics::new();
        let listener_metrics = metrics
            .register_listener(
                &spec.name,
                spec.kind.protocol(),
                spec.bind.to_string(),
                spec.max_connections.unwrap_or(u64::MAX),
            )
            .expect("listener metrics");
        let downstream_timeouts = spec.downstream_timeouts;
        let ServiceKind::Http(service) = spec.kind else {
            panic!("HTTP service expected");
        };
        let configuration = Arc::new(ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        });
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
            loop {
                let (stream, _) = listener.accept().await.expect("proxy accept");
                let Some(admission) = proxy.admit_connection() else {
                    continue;
                };
                let mut stream = pingora::protocols::l4::stream::Stream::from(stream);
                stream.set_socket_digest(SocketDigest::from_raw_fd(stream.as_raw_fd()));
                let stream: pingora::protocols::Stream = Box::new(stream);
                let proxy = Arc::clone(&proxy);
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    let _admission = admission;
                    proxy.process_new(stream, &shutdown).await;
                });
            }
        });
        Self {
            address,
            pool,
            origins,
            access_log,
            _directory: directory,
            _generation: generation,
            _shutdown: shutdown_tx,
            task,
        }
    }

    pub fn get(&self, request_id: &str) -> JoinHandle<HttpResponse> {
        self.request("GET", request_id, &[], b"")
    }

    pub fn request(
        &self,
        method: &str,
        request_id: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> JoinHandle<HttpResponse> {
        let address = self.address;
        let method = method.to_owned();
        let headers = std::iter::once(("X-Request-Id", request_id))
            .chain(headers.iter().copied())
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<Vec<_>>();
        let body = body.to_vec();
        tokio::spawn(async move {
            let headers = headers
                .iter()
                .map(|(n, v)| (n.as_str(), v.as_str()))
                .collect::<Vec<_>>();
            http_request(address, &method, "/render", &headers, &body).await
        })
    }

    pub fn evacuate_all(&self, force: bool) -> EvacuationReport {
        self.pool.evacuate_all_servers(force)
    }

    pub fn evacuate(&self, server: &str, force: bool) -> EvacuationReport {
        self.pool
            .evacuate_server(server, force)
            .expect("known server")
    }

    /// The access log lines written so far, waiting for at least `lines` of them.
    pub async fn access_events(&self, lines: usize) -> Vec<serde_json::Value> {
        timeout(TEST_TIMEOUT, async {
            loop {
                let content = std::fs::read_to_string(&self.access_log).unwrap_or_default();
                if content.lines().count() >= lines {
                    return content
                        .lines()
                        .map(|line| serde_json::from_str(line).expect("log line"))
                        .collect();
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("access log lines in time")
    }
}

impl Drop for Farm {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn config(
    layout: &Layout,
    address: SocketAddr,
    origins: &[Origin],
    access_log: Option<PathBuf>,
) -> ConfigDraft {
    let servers = origins
        .iter()
        .enumerate()
        .map(|(index, origin)| UpstreamServer {
            name: server_name(index),
            endpoint: socket_endpoint(origin.address),
            max_connections: layout.capacity,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        })
        .collect();
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        retirement: None,
        listeners: vec![Listener {
            name: "http-loopback".into(),
            bind: socket_bind(address),
            protocol: Protocol::Http,
            service: Some("evacuation".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(1_000),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        upstream_pools: vec![UpstreamPool {
            name: layout.pool.into(),
            servers,
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::First,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: layout.queue_ms,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: PoolResilience::default(),
            queue: None,
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "evacuation".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy {
                    request_buffering: layout.replayable_posts,
                    ..HttpRoutePolicy::default()
                },
                action: HttpRouteAction::Proxy {
                    upstream_pool: layout.pool.into(),
                    policy: HttpProxyPolicy {
                        retry: layout.retry(),
                        ..HttpProxyPolicy::default()
                    }
                    .into(),
                },
            }],
            automatic_response_headers: false,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(1024),
            gzip: None,
            access_log: access_log.map(|path| AccessLogPolicy::File { path }),
        }],
        ..empty_config()
    }
}

pub async fn wait_for_live_phase(request_id: &str, phase: &str) {
    timeout(TEST_TIMEOUT, async {
        while !oxiroute_server::live_requests()
            .iter()
            .any(|(id, live)| id == request_id && live == phase)
        {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("request {request_id} never reached {phase}"));
}

pub fn assert_report(
    report: &EvacuationReport,
    redispatched: u64,
    forced: u64,
    left: &[(LeftRunning, u64)],
) {
    assert_eq!(report.redispatched, redispatched, "{report:?}");
    assert_eq!(report.forced, forced, "{report:?}");
    assert_eq!(
        report.left_running,
        left.iter().copied().collect(),
        "{report:?}"
    );
}

pub fn assert_served_by(response: &HttpResponse, server: &str, request_id: &str) {
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(
        response.header("x-origin"),
        Some(server),
        "{}",
        response.text()
    );
    assert_eq!(response.body(), format!("from-{server}").as_bytes());
    assert_eq!(response.header("x-request-id"), Some(request_id));
}
