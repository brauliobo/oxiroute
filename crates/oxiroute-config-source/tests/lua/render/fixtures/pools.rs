use std::{net::SocketAddr, path::PathBuf};

use oxiroute_config::{
    CircuitBreaker, DnsResolutionPolicy, HealthCheck, HealthCheckType, HealthStartup,
    HttpRetryBodySafety, HttpRetryBudget, HttpRetryMethodSafety, HttpRetryPolicy, HttpRetryTarget,
    HttpRetryTrigger, HttpVersion, HttpVersionPolicy, OutlierDetection, PassiveHealthPolicy,
    PassiveObserve, PassiveOnError, PoolQueueClass, PoolQueuePolicy, QueueDiscipline, Readmission,
    ReadmissionProbe, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
    UpstreamServer, UpstreamTls,
};

pub(crate) fn socket(value: &str) -> SocketAddr {
    value.parse().expect("valid test socket address")
}

pub(crate) fn server(name: &str, endpoint: UpstreamEndpoint) -> UpstreamServer {
    UpstreamServer {
        name: name.into(),
        endpoint,
        max_connections: None,
        dns_resolution: DnsResolutionPolicy::OnConnect,
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn test_upstream_pools() -> Vec<UpstreamPool> {
    vec![
        UpstreamPool {
            name: "secure-backends".into(),
            servers: vec![
                server(
                    "secure-ip",
                    UpstreamEndpoint::Socket {
                        address: socket("192.0.2.88:443"),
                    },
                ),
                server(
                    "secure-dns",
                    UpstreamEndpoint::Dns {
                        host: "SECURE.EXAMPLE.TEST".into(),
                        port: 443,
                    },
                ),
            ],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: Some(PassiveHealthPolicy {
                observe: PassiveObserve::Layer4,
                on_error: PassiveOnError::MarkDown,
                error_limit: 4,
                mark_down: true,
                mark_up: true,
                initial_backoff_ms: 10_000,
                max_backoff_ms: 60_000,
                recovery_threshold: 2,
                readmission: Some(Readmission {
                    successes: 3,
                    interval_ms: 1_500,
                    timeout_ms: 800,
                    probe: Some(ReadmissionProbe {
                        path: "/healthz/deep".into(),
                        host: None,
                        expected_status: Some(204),
                    }),
                }),
                circuit_breaker: Some(CircuitBreaker {
                    window_ms: 20_000,
                    min_requests: 10,
                    failure_ratio_percent: 40,
                    open_ms: 5_000,
                    half_open_max: 2,
                    half_open_successes: 4,
                }),
                outlier_detection: Some(OutlierDetection {
                    factor_percent: 250,
                    min_samples: 30,
                    interval_ms: 5_000,
                    max_ejected_percent: 40,
                }),
            }),
            tls: Some(UpstreamTls {
                server_name: Some("ORIGIN.EXAMPLE.TEST".into()),
                verify: oxiroute_config::UpstreamTlsVerify::Full,
                ca_certificate_path: Some(PathBuf::from("/etc/oxiroute/origin-\"ca\".pem")),
            }),
            http_versions: HttpVersionPolicy {
                min: HttpVersion::Http11,
                max: HttpVersion::Http2,
            },
            queue_timeout_ms: Some(5_000),
            connect_timeout_ms: Some(10_000),
            server_timeout_ms: Some(30_000),
            connection_reuse: UpstreamConnectionReuse::Safe,
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: Some(test_queue_policy()),
        },
        UpstreamPool {
            name: "web-backends".into(),
            servers: vec![
                server(
                    "web-ip",
                    UpstreamEndpoint::Socket {
                        address: socket("127.0.0.1:3001"),
                    },
                ),
                server(
                    "web-dns",
                    UpstreamEndpoint::Dns {
                        host: "WEB.EXAMPLE.TEST".into(),
                        port: 3000,
                    },
                ),
            ],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::LeastConnections,
            health_check: Some(HealthCheck {
                kind: HealthCheckType::Http,
                interval_ms: 5_000,
                timeout_ms: 750,
                healthy_threshold: 2,
                unhealthy_threshold: 4,
                startup: HealthStartup::default(),
                fast_interval_ms: Some(2_000),
                down_interval_ms: Some(20_000),
                host: Some("backend.internal:3000".into()),
                path: Some("/healthz".into()),
                expected_status: Some(200),
                http_version: Some(oxiroute_config::HealthHttpVersion::Http11),
            }),
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: Some(2_000),
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Safe,
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: Some(test_queue_policy()),
        },
        UpstreamPool {
            name: "database-backends".into(),
            servers: vec![server(
                "database",
                UpstreamEndpoint::Socket {
                    address: socket("192.0.2.58:5432"),
                },
            )],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: Some(HealthCheck {
                kind: HealthCheckType::Tcp,
                interval_ms: 10_000,
                timeout_ms: 1_000,
                healthy_threshold: 1,
                unhealthy_threshold: 3,
                startup: HealthStartup::default(),
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
            connection_reuse: UpstreamConnectionReuse::Never,
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        },
        UpstreamPool {
            name: "unix-backends".into(),
            servers: vec![server(
                "unix",
                UpstreamEndpoint::Unix {
                    path: "/run//oxiroute/backend.sock".into(),
                },
            )],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Never,
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        },
    ]
}

fn test_queue_policy() -> PoolQueuePolicy {
    PoolQueuePolicy {
        classes: vec![
            PoolQueueClass {
                name: "interactive".into(),
                weight: 8,
            },
            PoolQueueClass {
                name: "batch".into(),
                weight: 1,
            },
        ],
        discipline: QueueDiscipline::Strict,
        class_header: Some("x-priority".into()),
        default_class: Some("batch".into()),
        tenant_header: Some("x-tenant".into()),
        max_tenants: 16,
        aging_ms: Some(30_000),
    }
}

pub(crate) fn test_retry_policy() -> HttpRetryPolicy {
    HttpRetryPolicy {
        max_retries: 2,
        triggers: vec![
            HttpRetryTrigger::ConnectTimeout,
            HttpRetryTrigger::RefusedStream,
        ],
        response_statuses: Vec::new(),
        method_safety: HttpRetryMethodSafety::GetHead,
        body_safety: HttpRetryBodySafety::Empty,
        target: HttpRetryTarget::SameServer,
        delay_ms: 25,
        jitter_percent: 40,
        final_redispatch: false,
        idempotency_header: None,
        budget: Some(HttpRetryBudget {
            per_second: Some(20),
            ratio_percent: Some(25),
        }),
    }
}
