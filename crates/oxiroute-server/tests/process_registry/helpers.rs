#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn registered_pool(name: &str) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: Vec::new(),
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: Some(HealthCheck {
            kind: HealthCheckType::Tcp,
            interval_ms: 1_000,
            timeout_ms: 500,
            healthy_threshold: 1,
            unhealthy_threshold: 1,
            startup: HealthStartup::Healthy,
            fast_interval_ms: None,
            down_interval_ms: None,
            host: None,
            path: None,
            expected_status: None,
            http_version: None,
        }),
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: Some(PoolRegistration {
            allowed_cidrs: vec!["127.0.0.0/8".into()],
            require_peer_address: false,
            min_ttl_ms: 2_000,
            default_ttl_ms: 3_000,
            ..PoolRegistration::default()
        }),
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
        canary: None,
    }
}

pub(super) fn route(path: &str, pool: &str) -> HttpRoute {
    HttpRoute {
        host: None,
        path: HttpPathSelector::SegmentPrefix { value: path.into() },
        methods: Vec::new(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::Proxy {
            upstream_pool: pool.into(),
            policy: HttpProxyPolicy::default(),
        },
    }
}

pub(super) async fn eventually(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
