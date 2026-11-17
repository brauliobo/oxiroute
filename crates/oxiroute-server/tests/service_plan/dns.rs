use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, HealthCheck, HealthCheckType, HealthHttpVersion,
    HttpVersionPolicy, Stats, StatsPage, StatsPageAdminPolicy, UpstreamAlgorithm, UpstreamEndpoint,
    UpstreamPool, UpstreamServer,
};

use crate::config_support::{self, empty_config, runtime_plan};

#[test]
fn startup_dns_cannot_resolve_to_a_statistics_listener() {
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: vec!["127.0.0.1:18404".parse().expect("stats bind")],
        admin_token_file: None,
        pages: Vec::new(),
    });
    config.upstream_pools.push(UpstreamPool {
        name: "protected".into(),
        servers: vec![UpstreamServer {
            name: "stats-rebind".into(),
            endpoint: UpstreamEndpoint::Dns {
                host: "localhost".into(),
                port: 18404,
            },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::Startup,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });

    let config = config.validate().expect("valid runtime config");
    let Err(error) = runtime_plan(&config) else {
        panic!("protected startup DNS must fail")
    };
    assert!(
        error
            .to_string()
            .contains("protected management or statistics listener")
    );
}

#[test]
fn upstream_socket_cannot_target_a_statistics_page_listener() {
    let page_bind = "127.0.0.1:18405".parse().expect("page bind");
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: page_bind,
            uri_prefix: "/stats".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
    });
    config.upstream_pools.push(UpstreamPool {
        name: "protected-page".into(),
        servers: vec![UpstreamServer {
            name: "page-loop".into(),
            endpoint: UpstreamEndpoint::Socket { address: page_bind },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });

    let config = config.validate().expect("valid runtime config");
    let Err(error) = runtime_plan(&config) else {
        panic!("protected statistics page destination must fail")
    };
    assert!(
        error
            .to_string()
            .contains("protected management or statistics listener")
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one integration case keeps all three audited host pool shapes visible"
)]
fn dns_pool_shapes_compile_with_named_policy() {
    let haproxy_http_check = HealthCheck {
        kind: HealthCheckType::Http,
        interval_ms: 2_000,
        timeout_ms: 1_000,
        healthy_threshold: 2,
        unhealthy_threshold: 3,
        startup: oxiroute_config::HealthStartup::Checking,
        fast_interval_ms: Some(1_000),
        down_interval_ms: Some(5_000),
        host: None,
        path: Some("/healthz".into()),
        expected_status: Some(200),
        http_version: Some(HealthHttpVersion::Http10),
    };
    let dns_server = |name: String, port| UpstreamServer {
        endpoint: UpstreamEndpoint::Dns {
            host: format!("{name}.test"),
            port,
        },
        name,
        max_connections: Some(100),
        dns_resolution: DnsResolutionPolicy::OnConnect,
    };
    let pool = |name: &str,
                servers: Vec<UpstreamServer>,
                algorithm,
                health_check: Option<HealthCheck>| UpstreamPool {
        name: name.into(),
        servers,
        endpoints: Vec::new(),
        algorithm,
        health_check,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: Some(5_000),
        connect_timeout_ms: Some(5_000),
        server_timeout_ms: Some(50_000),
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    };
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
        max_connections: Some(4_096),
        upstream_pools: vec![
            pool(
                "app-pool",
                vec![
                    dns_server("app-pool01".into(), 3_000),
                    dns_server("app-pool02".into(), 3_000),
                ],
                UpstreamAlgorithm::First,
                Some(haproxy_http_check.clone()),
            ),
            pool(
                "app_nodes",
                vec![
                    dns_server("app01".into(), 3_000),
                    dns_server("app02".into(), 3_000),
                ],
                UpstreamAlgorithm::LeastConnections,
                Some(haproxy_http_check.clone()),
            ),
            pool(
                "media_nodes",
                (1..=8)
                    .map(|ordinal| dns_server(format!("media{ordinal:02}"), 4_000))
                    .collect(),
                UpstreamAlgorithm::LeastConnections,
                Some(haproxy_http_check),
            ),
        ],
        ..empty_config()
    };

    let validated = config.clone().validate().unwrap();
    let plan = runtime_plan(&validated).expect("host-shaped runtime plan");
    let generation =
        config_support::runtime_generation(&validated).expect("host-shaped runtime generation");
    assert_eq!(plan.max_connections, Some(4_096));
    assert!(generation.health_supervisor().is_some());
    assert_eq!(
        generation
            .pools()
            .iter()
            .map(|pool| {
                let snapshot = pool.health_snapshot();
                (
                    snapshot.name,
                    snapshot.algorithm,
                    snapshot
                        .endpoints
                        .into_iter()
                        .map(|server| server.name)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "app-pool".into(),
                "first",
                vec!["app-pool01".into(), "app-pool02".into()]
            ),
            (
                "app_nodes".into(),
                "least_connections",
                vec!["app01".into(), "app02".into()]
            ),
            (
                "media_nodes".into(),
                "least_connections",
                (1..=8)
                    .map(|ordinal| format!("media{ordinal:02}"))
                    .collect()
            ),
        ]
    );
}
