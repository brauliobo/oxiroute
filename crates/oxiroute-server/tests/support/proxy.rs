#[derive(Clone, Copy)]
pub struct ReservedListener {
    pub address: SocketAddr,
}

impl ReservedListener {
    pub fn new() -> Self {
        Self {
            address: ports::unused_address(),
        }
    }
}

pub struct ProxyConfig {
    config: ConfigDraft,
    directory: TempDir,
}

pub struct ValidatedProxyConfig {
    config: ValidatedConfig,
    _directory: TempDir,
}

impl std::ops::Deref for ValidatedProxyConfig {
    type Target = ValidatedConfig;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl std::ops::Deref for ProxyConfig {
    type Target = ConfigDraft;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl std::ops::DerefMut for ProxyConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.config
    }
}

impl ProxyConfig {
    pub fn validate(self) -> ValidatedProxyConfig {
        ValidatedProxyConfig {
            config: self.config.validate().expect("valid wire config"),
            _directory: self.directory,
        }
    }
}

/// The default policy of the wire tests. `OXIROUTE_INTERNAL_TEST_TLS_BACKEND=rustls` runs the same tests against the
/// rustls backend (`scripts/test-tls-backends.sh` runs both).
pub fn test_tls_policy() -> oxiroute_config::TlsPolicy {
    let backend = match std::env::var("OXIROUTE_INTERNAL_TEST_TLS_BACKEND").as_deref() {
        Ok("rustls") => oxiroute_config::TlsBackend::Rustls,
        _ => oxiroute_config::TlsBackend::Openssl,
    };
    oxiroute_config::TlsPolicy {
        backend,
        ..oxiroute_config::TlsPolicy::default()
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
    let config = ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
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
            policy: test_tls_policy(),
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
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "wire".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "origin".into(),
                    policy: HttpProxyPolicy::default().into(),
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
    ProxyConfig { config, directory }
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
    _generation: Arc<RuntimeGeneration>,
    active_certificates: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
    certbot_reconcilers: BTreeMap<String, Arc<CertbotReconciler>>,
    certbot_watcher: Option<CertbotWatcherSupervisor>,
    runtime_metrics: RuntimeMetrics,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ProxyHarness {
    pub fn start(config: &ValidatedConfig, reserved: ReservedListener) -> Self {
        let ReservedListener { address } = reserved;
        let generation = config::runtime_generation(config).expect("wire runtime generation");
        let tls = generation.tls();
        let certbot_reconcilers = tls
            .certbot_reconcilers()
            .iter()
            .map(|reconciler| (reconciler.status().certificate, Arc::clone(reconciler)))
            .collect::<BTreeMap<_, _>>();
        let certbot_watcher = tls
            .start_certbot_watcher(CertbotWatcherConfig::default())
            .expect("wire Certbot watcher");
        let active_certificates = tls.certificates().clone();
        let tls_alpn_challenges = tls.tls_alpn_challenge_store().clone();
        let active_certificate = Arc::clone(
            active_certificates
                .get("downstream")
                .expect("downstream certificate generation"),
        );
        let spec = generation.services().first().expect("wire runtime service");
        assert_eq!(spec.bind, ListenerBind::Socket { address });
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
        let ServiceKind::Http(http_service) = &spec.kind else {
            panic!("wire listener must compile as HTTP");
        };
        let server_configuration = Arc::new(ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        });
        let app = HttpListenerApp::new(
            http_proxy(
                &server_configuration,
                HttpReverseProxy::new(Arc::clone(http_service), listener_metrics.clone())
                    .with_generation(Arc::clone(&generation)),
            ),
            spec.tls.as_deref(),
        )
        .with_generation(Arc::clone(&generation));
        let tls = spec.tls.as_ref().expect("downstream TLS profile");
        let app = MonitoredHttpApp::new(app, listener_metrics, Arc::clone(&generation));
        let mut service = ListeningService::new("OxiRoute wire test".into(), app);
        tls.listen(&mut service, &bind, 2)
            .expect("downstream TLS settings");

        let inherited = generation
            .reservations()
            .get("wire")
            .expect("wire listener reservation")
            .duplicate_fds()
            .expect("duplicate wire listener reservation");
        let inherited = Arc::new(parking_lot::Mutex::new(inherited));
        let (shutdown, shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            PingoraService::start_service(&mut service, Some(inherited), shutdown_watch, 1).await;
        });

        Self {
            address,
            active_certificate,
            tls_alpn_challenges,
            _generation: generation,
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
