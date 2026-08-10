#[path = "support/config.rs"]
mod config_support;
#[path = "support/http.rs"]
mod http_support;

use std::{net::SocketAddr, path::Path, time::Duration};

use oxiroute_config::{
    Config, ConfigError, HealthCheck, HealthCheckType, HealthHttpVersion, HttpVersionPolicy,
    UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool,
};
use oxiroute_import::haproxy::import_roots;
use oxiroute_server::{
    EndpointHealthState, HealthFailure, RuntimeMetrics, ServicePlanError, runtime_plan,
};
use pingora::services::{ServiceReadyNotifier, background::BackgroundService};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::watch,
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_endpoint};
use http_support::read_request_head as read_request_head_bytes;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn imported_haproxy_http_check_send_compiles_into_the_runtime_health_plan() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/haproxy/http-check-send.cfg");
    let imported = import_roots(&[path]);
    assert!(!imported.has_errors(), "{:?}", imported.diagnostics());
    let config = imported.value().config().expect("imported config");
    let plan = runtime_plan(config).expect("runtime plan");
    let health = config.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("canonical health check");

    assert_eq!(health.path.as_deref(), Some("/healthz"));
    assert_eq!(health.host.as_deref(), Some("backend.internal"));
    assert_eq!(health.expected_status, Some(204));
    assert_eq!(plan.pools[0].health_snapshot().endpoints.len(), 1);
}

#[tokio::test]
async fn tcp_probe_establishes_healthy_and_unhealthy_states() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("health bind");
        let healthy_address = listener.local_addr().expect("health address");
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await.expect("health accept");
        });
        let healthy_plan = runtime_plan(&config(
            socket_endpoint(healthy_address),
            tcp_policy(1_000, 200, 1, 1),
        ))
        .expect("healthy runtime plan");
        let healthy_pool = &healthy_plan.pools[0];
        assert!(healthy_pool.select().is_none());

        healthy_plan
            .health_supervisor
            .expect("health supervisor")
            .probe_once()
            .await;
        accept.await.expect("accept task");
        assert_eq!(
            healthy_pool
                .select()
                .map(|lease| lease.endpoint().to_string()),
            Some(healthy_address.to_string())
        );
        assert_eq!(
            healthy_pool.health_snapshot().endpoints[0].state,
            EndpointHealthState::Healthy
        );

        let unavailable_address = unused_address().await;
        let unhealthy_plan = runtime_plan(&config(
            socket_endpoint(unavailable_address),
            tcp_policy(1_000, 200, 1, 1),
        ))
        .expect("unhealthy runtime plan");
        let unhealthy_pool = &unhealthy_plan.pools[0];
        unhealthy_plan
            .health_supervisor
            .expect("health supervisor")
            .probe_once()
            .await;
        assert!(unhealthy_pool.select().is_none());
        let snapshot = unhealthy_pool.health_snapshot();
        assert_eq!(snapshot.unavailable_selections, 1);
        assert_eq!(snapshot.endpoints[0].state, EndpointHealthState::Unhealthy);
        assert_eq!(
            snapshot.endpoints[0].last_failure,
            Some(HealthFailure::ConnectFailed)
        );

        let metrics = RuntimeMetrics::new();
        metrics
            .register_upstream_pools(unhealthy_plan.pools.clone())
            .expect("pool metrics");
        let json = serde_json::to_value(metrics.snapshot().expect("runtime snapshot"))
            .expect("snapshot JSON");
        assert_eq!(json["upstreamPools"][0]["name"], "checked");
        assert_eq!(json["upstreamPools"][0]["availableEndpoints"], 0);
        assert_eq!(json["upstreamPools"][0]["unavailableSelections"], "1");
        assert_eq!(
            json["upstreamPools"][0]["endpoints"][0]["state"],
            "unhealthy"
        );
        assert_eq!(
            json["upstreamPools"][0]["endpoints"][0]["failedChecks"],
            "1"
        );
    })
    .await
    .expect("TCP health test timed out");
}

#[tokio::test]
async fn http_probe_sends_the_configured_host_and_path() {
    timeout(TEST_TIMEOUT, async {
        let (address, server) = http_origin(200, false).await;
        let plan = runtime_plan(&config(socket_endpoint(address), http_policy(1_000, 300)))
            .expect("HTTP health runtime plan");

        plan.health_supervisor
            .expect("health supervisor")
            .probe_once()
            .await;
        let request = server.await.expect("health origin");

        assert!(
            request.starts_with("GET /healthz HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.contains("\r\nHost: backend.internal\r\n"),
            "{request}"
        );
        assert_eq!(
            plan.pools[0].health_snapshot().endpoints[0].state,
            EndpointHealthState::Healthy
        );
    })
    .await
    .expect("HTTP health test timed out");
}

#[tokio::test]
async fn http_probe_honors_http_10_optional_host_and_exact_status() {
    timeout(TEST_TIMEOUT, async {
        let (address, server) = http_origin(204, false).await;
        let mut policy = http_policy(1_000, 300);
        policy.host = None;
        policy.expected_status = Some(204);
        policy.http_version = Some(HealthHttpVersion::Http10);
        let plan = runtime_plan(&config(socket_endpoint(address), policy))
            .expect("HTTP/1.0 health runtime plan");

        plan.health_supervisor
            .expect("health supervisor")
            .probe_once()
            .await;
        let request = server.await.expect("health origin");

        assert!(
            request.starts_with("GET /healthz HTTP/1.0\r\n"),
            "{request}"
        );
        assert!(
            !request
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("host:")),
            "{request}"
        );
        assert_eq!(
            plan.pools[0].health_snapshot().endpoints[0].state,
            EndpointHealthState::Healthy
        );
    })
    .await
    .expect("HTTP/1.0 health test timed out");
}

#[tokio::test]
async fn http_probe_reports_status_failure_and_total_timeout() {
    timeout(TEST_TIMEOUT, async {
        let (failed_address, failed_server) = http_origin(503, false).await;
        let failed_plan = runtime_plan(&config(
            socket_endpoint(failed_address),
            http_policy(1_000, 300),
        ))
        .expect("failed HTTP health plan");
        failed_plan
            .health_supervisor
            .expect("health supervisor")
            .probe_once()
            .await;
        failed_server.await.expect("failed health origin");
        assert_eq!(
            failed_plan.pools[0].health_snapshot().endpoints[0].last_failure,
            Some(HealthFailure::UnexpectedStatus)
        );

        let (slow_address, slow_server) = http_origin(200, true).await;
        let slow_plan = runtime_plan(&config(
            socket_endpoint(slow_address),
            http_policy(1_000, 100),
        ))
        .expect("slow HTTP health plan");
        slow_plan
            .health_supervisor
            .expect("health supervisor")
            .probe_once()
            .await;
        slow_server.abort();
        assert_eq!(
            slow_plan.pools[0].health_snapshot().endpoints[0].last_failure,
            Some(HealthFailure::Timeout)
        );
    })
    .await
    .expect("HTTP health failure test timed out");
}

#[tokio::test]
async fn supervisor_signals_readiness_immediately_and_stops_while_sleeping() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("health bind");
        let address = listener.local_addr().expect("health address");
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await.expect("health accept");
        });
        let plan = runtime_plan(&config(
            socket_endpoint(address),
            tcp_policy(60_000, 500, 1, 1),
        ))
        .expect("health runtime plan");
        let supervisor = plan.health_supervisor.expect("health supervisor");
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (ready_tx, mut ready) = watch::channel(false);
        let supervisor_task = tokio::spawn(async move {
            supervisor
                .start_with_ready_notifier(shutdown, ServiceReadyNotifier::new(ready_tx))
                .await;
        });

        ready.changed().await.expect("readiness change");
        assert!(*ready.borrow());
        accept.await.expect("accept task");
        shutdown_tx.send(true).expect("shutdown signal");
        timeout(Duration::from_millis(200), supervisor_task)
            .await
            .expect("sleeping supervisor must stop promptly")
            .expect("supervisor task");
    })
    .await
    .expect("health lifecycle test timed out");
}

#[tokio::test]
async fn health_groups_schedule_independently() {
    timeout(TEST_TIMEOUT, async {
        let fast_listener = TcpListener::bind("127.0.0.1:0").await.expect("fast bind");
        let fast_address = fast_listener.local_addr().expect("fast address");
        let fast_accepts = tokio::spawn(async move {
            for _ in 0..2 {
                let _ = fast_listener.accept().await.expect("fast accept");
            }
        });
        let (slow_address, slow_server) = http_origin(200, true).await;
        let plan = runtime_plan(&Config {
            upstream_pools: vec![
                pool(
                    "fast",
                    socket_endpoint(fast_address),
                    tcp_policy(1_000, 200, 1, 1),
                ),
                pool(
                    "slow",
                    socket_endpoint(slow_address),
                    http_policy(2_000, 1_500),
                ),
            ],
            ..empty_config()
        })
        .expect("independent health plan");
        let supervisor = plan.health_supervisor.expect("health supervisor");
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (ready_tx, _ready) = watch::channel(false);
        let supervisor_task = tokio::spawn(async move {
            supervisor
                .start_with_ready_notifier(shutdown, ServiceReadyNotifier::new(ready_tx))
                .await;
        });

        timeout(Duration::from_millis(1_400), fast_accepts)
            .await
            .expect("slow group must not delay the fast group's next probe")
            .expect("fast accepts");
        shutdown_tx.send(true).expect("shutdown signal");
        supervisor_task.await.expect("supervisor task");
        slow_server.abort();
    })
    .await
    .expect("independent scheduling test timed out");
}

#[tokio::test]
async fn endpoints_in_one_pool_schedule_from_their_own_completion() {
    timeout(TEST_TIMEOUT, async {
        let fast_listener = TcpListener::bind("127.0.0.1:0").await.expect("fast bind");
        let fast_address = fast_listener.local_addr().expect("fast address");
        let fast_accepts = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = fast_listener.accept().await.expect("fast accept");
                read_request_head(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .expect("fast response");
            }
        });
        let (slow_address, slow_server) = http_origin(200, true).await;
        let plan = runtime_plan(&Config {
            upstream_pools: vec![UpstreamPool {
                name: "mixed".into(),
                servers: Vec::new(),
                endpoints: vec![socket_endpoint(fast_address), socket_endpoint(slow_address)],
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: Some(http_policy(1_000, 900)),
                passive_health: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
            }],
            ..empty_config()
        })
        .expect("mixed health plan");
        let supervisor = plan.health_supervisor.expect("health supervisor");
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (ready_tx, _ready) = watch::channel(false);
        let supervisor_task = tokio::spawn(async move {
            supervisor
                .start_with_ready_notifier(shutdown, ServiceReadyNotifier::new(ready_tx))
                .await;
        });

        timeout(Duration::from_millis(1_400), fast_accepts)
            .await
            .expect("a slow peer must not delay the fast peer's next probe")
            .expect("fast accepts");
        shutdown_tx.send(true).expect("shutdown signal");
        supervisor_task.await.expect("supervisor task");
        slow_server.abort();
    })
    .await
    .expect("same-pool scheduling test timed out");
}

#[tokio::test]
async fn supervisor_honors_shutdown_already_requested_at_startup() {
    let address = unused_address().await;
    let plan = runtime_plan(&config(
        socket_endpoint(address),
        tcp_policy(60_000, 500, 1, 1),
    ))
    .expect("health runtime plan");
    let supervisor = plan.health_supervisor.expect("health supervisor");
    let (_shutdown_tx, shutdown) = watch::channel(true);
    let (ready_tx, _ready) = watch::channel(false);

    timeout(
        Duration::from_millis(200),
        supervisor.start_with_ready_notifier(shutdown, ServiceReadyNotifier::new(ready_tx)),
    )
    .await
    .expect("pre-signaled shutdown must stop the supervisor");
}

#[tokio::test]
async fn runtime_plan_validates_programmatic_health_policies() {
    let address = unused_address().await;
    let result = runtime_plan(&config(
        socket_endpoint(address),
        tcp_policy(999, 200, 1, 1),
    ));

    assert!(matches!(
        result,
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(source.as_ref(), ConfigError::InvalidHealthCheck { .. })
    ));
}

#[tokio::test]
async fn runtime_plan_enforces_programmatic_endpoint_cardinality() {
    let endpoints = (10_000..10_257)
        .map(|port| socket_endpoint(SocketAddr::from(([127, 0, 0, 1], port))))
        .collect();
    let result = runtime_plan(&Config {
        upstream_pools: vec![UpstreamPool {
            name: "oversized".into(),
            servers: Vec::new(),
            endpoints,
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: Some(tcp_policy(1_000, 200, 1, 1)),
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        }],
        ..empty_config()
    });

    assert!(matches!(
        result,
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::TooManyUpstreamEndpoints { pool } if pool == "oversized"
            )
    ));
}

#[tokio::test]
async fn dns_tcp_probe_resolves_at_probe_time_and_preserves_dns_identity() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("DNS health bind");
        let port = listener.local_addr().expect("DNS health address").port();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await.expect("DNS health accept");
        });
        let endpoint = UpstreamEndpoint::Dns {
            host: "localhost".into(),
            port,
        };
        let plan = runtime_plan(&config(endpoint, tcp_policy(1_000, 300, 1, 1)))
            .expect("DNS TCP health plan");

        plan.health_supervisor
            .as_ref()
            .expect("DNS health supervisor")
            .probe_once()
            .await;
        accept.await.expect("DNS health accept task");

        let snapshot = plan.pools[0].health_snapshot();
        assert_eq!(
            snapshot.endpoints[0].address.to_string(),
            format!("localhost:{port}")
        );
        assert_eq!(snapshot.endpoints[0].state, EndpointHealthState::Healthy);
    })
    .await
    .expect("DNS TCP health test timed out");
}

#[tokio::test]
async fn dns_http_probe_resolves_fresh_and_sends_the_configured_request() {
    timeout(TEST_TIMEOUT, async {
        let (address, server) = http_origin(200, false).await;
        let endpoint = UpstreamEndpoint::Dns {
            host: "localhost".into(),
            port: address.port(),
        };
        let plan =
            runtime_plan(&config(endpoint, http_policy(1_000, 300))).expect("DNS HTTP health plan");

        plan.health_supervisor
            .as_ref()
            .expect("DNS health supervisor")
            .probe_once()
            .await;
        let request = server.await.expect("DNS HTTP health origin");

        assert!(request.starts_with("GET /healthz HTTP/1.1\r\n"));
        assert!(request.contains("\r\nHost: backend.internal\r\n"));
        assert_eq!(
            plan.pools[0].health_snapshot().endpoints[0].state,
            EndpointHealthState::Healthy
        );
    })
    .await
    .expect("DNS HTTP health test timed out");
}

#[tokio::test]
async fn dns_no_answer_is_a_bounded_connect_failure() {
    let endpoint = UpstreamEndpoint::Dns {
        host: "no-answer.invalid".into(),
        port: 80,
    };
    let plan = runtime_plan(&config(endpoint, tcp_policy(1_000, 100, 1, 1)))
        .expect("no-answer health plan");

    timeout(
        Duration::from_millis(300),
        plan.health_supervisor
            .as_ref()
            .expect("no-answer supervisor")
            .probe_once(),
    )
    .await
    .expect("DNS no-answer probe exceeded its bound");
    assert_eq!(
        plan.pools[0].health_snapshot().endpoints[0].last_failure,
        Some(HealthFailure::ConnectFailed)
    );
}

fn config(endpoint: UpstreamEndpoint, health_check: HealthCheck) -> Config {
    Config {
        upstream_pools: vec![pool("checked", endpoint, health_check)],
        ..empty_config()
    }
}

fn pool(name: &str, endpoint: UpstreamEndpoint, health_check: HealthCheck) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: Vec::new(),
        endpoints: vec![endpoint],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: Some(health_check),
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
    }
}

fn tcp_policy(
    interval_ms: u64,
    timeout_ms: u64,
    healthy_threshold: u16,
    unhealthy_threshold: u16,
) -> HealthCheck {
    HealthCheck {
        kind: HealthCheckType::Tcp,
        interval_ms,
        timeout_ms,
        healthy_threshold,
        unhealthy_threshold,
        startup: oxiroute_config::HealthStartup::default(),
        fast_interval_ms: None,
        down_interval_ms: None,
        host: None,
        path: None,
        expected_status: None,
        http_version: None,
    }
}

fn http_policy(interval_ms: u64, timeout_ms: u64) -> HealthCheck {
    HealthCheck {
        kind: HealthCheckType::Http,
        interval_ms,
        timeout_ms,
        healthy_threshold: 1,
        unhealthy_threshold: 1,
        startup: oxiroute_config::HealthStartup::default(),
        fast_interval_ms: None,
        down_interval_ms: None,
        host: Some("backend.internal".into()),
        path: Some("/healthz".into()),
        expected_status: None,
        http_version: None,
    }
}

async fn unused_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .await
        .expect("unused bind")
        .local_addr()
        .expect("unused address")
}

async fn http_origin(
    status: u16,
    stream_body_forever: bool,
) -> (SocketAddr, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("HTTP bind");
    let address = listener.local_addr().expect("HTTP address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("HTTP accept");
        let request = read_request_head(&mut stream).await;
        if stream_body_forever {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n")
                .await
                .expect("streaming response");
            loop {
                sleep(Duration::from_millis(25)).await;
                if stream.write_all(b"1\r\nx\r\n").await.is_err() {
                    break;
                }
            }
        } else {
            let response =
                format!("HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            stream
                .write_all(response.as_bytes())
                .await
                .expect("HTTP response");
        }
        request
    });
    (address, server)
}

async fn read_request_head(stream: &mut TcpStream) -> String {
    let request = read_request_head_bytes(stream).await.expect("request read");
    String::from_utf8(request).expect("ASCII request")
}
