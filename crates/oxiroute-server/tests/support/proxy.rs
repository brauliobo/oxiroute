pub struct ReservedListener {
    pub address: SocketAddr,
    listener: std::net::TcpListener,
}

impl ReservedListener {
    pub fn new() -> Self {
        let listener =
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve proxy listener");
        listener
            .set_nonblocking(true)
            .expect("make proxy listener nonblocking");
        let address = listener.local_addr().expect("reserved proxy address");
        Self { address, listener }
    }
}

pub struct ProxyConfig {
    config: Config,
    _directory: TempDir,
}

impl std::ops::Deref for ProxyConfig {
    type Target = Config;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl std::ops::DerefMut for ProxyConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.config
    }
}

pub fn proxy_config(
    listener_address: SocketAddr,
    origin_address: SocketAddr,
    downstream_alpn: Vec<AlpnProtocol>,
    upstream_tls: Option<UpstreamTls>,
    upstream_versions: HttpVersionPolicy,
) -> ProxyConfig {
    let directory = TempDir::new().expect("proxy config private-key directory");
    let private_key_path = copy_private_key_fixture(directory.path(), "proxy-a-key.pem");
    let config = Config {
        certificates: vec![Certificate {
            name: "downstream".into(),
            dns_names: vec![PROXY_SERVER_NAME.into()],
            source: CertificateSource::Files {
                certificate_chain_path: fixture("proxy-a.pem"),
                private_key_path,
            },
        }],
        tls_profiles: vec![TlsProfile {
            name: "downstream".into(),
            certificates: vec!["downstream".into()],
            default_certificate: "downstream".into(),
            min_version: TlsVersion::Tls12,
            alpn: downstream_alpn,
            policy: oxiroute_config::TlsPolicy::default(),
        }],
        listeners: vec![Listener {
            name: "wire".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::Http,
            service: Some("wire".into()),
            tls_profile: Some("downstream".into()),
            proxy_protocol: None,
            max_connections: Some(100),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
        upstream_pools: vec![UpstreamPool {
            name: "origin".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint(origin_address)],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: upstream_tls,
            http_versions: upstream_versions,
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        }],
        http_services: vec![HttpService {
            name: "wire".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "origin".into(),
                    policy: HttpProxyPolicy::default(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 2_000,
            max_request_body_bytes: Some(64 * 1024),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    };
    ProxyConfig {
        config,
        _directory: directory,
    }
}

pub fn verified_upstream(server_name: &str, ca_fixture: &str) -> UpstreamTls {
    UpstreamTls {
        server_name: server_name.into(),
        ca_certificate_path: Some(fixture(ca_fixture)),
    }
}

pub struct ProxyHarness {
    pub address: SocketAddr,
    pub active_certificate: Arc<ActiveCertificateGeneration>,
    pub tls_alpn_challenges: TlsAlpnChallengeStore,
    active_certificates: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
    certbot_reconcilers: BTreeMap<String, Arc<CertbotReconciler>>,
    certbot_watcher: Option<CertbotWatcherSupervisor>,
    runtime_metrics: RuntimeMetrics,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ProxyHarness {
    pub fn start(config: &Config, reserved: ReservedListener) -> Self {
        let mut plan = runtime_plan(config).expect("wire runtime plan");
        let certbot_reconcilers = plan
            .certbot_reconcilers()
            .iter()
            .map(|reconciler| (reconciler.status().certificate, Arc::clone(reconciler)))
            .collect::<BTreeMap<_, _>>();
        let certbot_watcher = plan
            .tls
            .start_certbot_watcher(CertbotWatcherConfig::default())
            .expect("wire Certbot watcher");
        let active_certificates = plan.tls.certificates().clone();
        let tls_alpn_challenges = plan.tls.tls_alpn_challenge_store().clone();
        let active_certificate = Arc::clone(
            active_certificates
                .get("downstream")
                .expect("downstream certificate generation"),
        );
        let spec = plan.services.remove(0);
        assert_eq!(
            spec.bind,
            ListenerBind::Socket {
                address: reserved.address,
            }
        );
        let bind = spec.bind.to_string();
        let metrics = RuntimeMetrics::new();
        metrics
            .register_certbot_monitoring(
                certbot_reconcilers.values().cloned(),
                certbot_watcher
                    .as_ref()
                    .map(CertbotWatcherSupervisor::monitor),
            )
            .expect("wire Certbot monitoring");
        let listener_metrics = metrics
            .register_listener(
                &spec.name,
                spec.kind.protocol(),
                bind.clone(),
                spec.max_connections.unwrap_or(u64::MAX),
            )
            .expect("wire listener metrics");
        let ServiceKind::Http(http_service) = spec.kind else {
            panic!("wire listener must compile as HTTP");
        };
        let server_configuration = Arc::new(ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        });
        let app = HttpListenerApp::new(
            http_proxy(
                &server_configuration,
                HttpReverseProxy::new(http_service, listener_metrics.clone()),
            ),
            spec.tls.as_deref(),
        );
        let tls = spec.tls.expect("downstream TLS profile");
        let app = MonitoredHttpApp::new(app, listener_metrics);
        let mut service = ListeningService::new("OxiRoute wire test".into(), app);
        service.add_tls_with_settings(
            &bind,
            None,
            tls.tls_settings().expect("downstream TLS settings"),
        );

        let mut inherited = Fds::new();
        inherited.add(bind, reserved.listener.into_raw_fd());
        let inherited = Arc::new(TokioMutex::new(inherited));
        let (shutdown, shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            PingoraService::start_service(&mut service, Some(inherited), shutdown_watch, 1).await;
        });

        Self {
            address: reserved.address,
            active_certificate,
            tls_alpn_challenges,
            active_certificates,
            certbot_reconcilers,
            certbot_watcher,
            runtime_metrics: metrics,
            shutdown,
            task: Some(task),
        }
    }

    pub fn certificate(&self, name: &str) -> &Arc<ActiveCertificateGeneration> {
        self.active_certificates
            .get(name)
            .unwrap_or_else(|| panic!("missing active certificate `{name}`"))
    }

    pub fn certbot_reconciler(&self, name: &str) -> &Arc<CertbotReconciler> {
        self.certbot_reconcilers
            .get(name)
            .unwrap_or_else(|| panic!("missing Certbot reconciler `{name}`"))
    }

    pub async fn wait_for_active_connections(&self, expected: u64) {
        timeout(IO_TIMEOUT, async {
            loop {
                let snapshot = self
                    .runtime_metrics
                    .snapshot()
                    .expect("wire runtime metrics snapshot");
                if snapshot.traffic.active_connections == expected {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("wire active connections did not reach {expected}"));
    }

    pub async fn finish(mut self) {
        self.shutdown.send(true).expect("signal proxy shutdown");
        let mut task = self.task.take().expect("proxy service task");
        if let Ok(result) = timeout(IO_TIMEOUT, &mut task).await {
            result.expect("proxy service task");
        } else {
            task.abort();
            let _ = task.await;
            panic!("proxy service did not stop within {IO_TIMEOUT:?}");
        }
        if let Some(watcher) = &mut self.certbot_watcher {
            watcher.shutdown();
        }
    }
}

impl Drop for ProxyHarness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
