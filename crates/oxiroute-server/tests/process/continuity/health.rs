use oxiroute_config::{
    DnsResolutionPolicy, DownstreamTimeoutPolicy, HealthCheck, HealthCheckType, HealthStartup,
    HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
    HttpVersionPolicy, Listener, PassiveHealthPolicy, PassiveObserve, PassiveOnError, Protocol,
    UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};

use super::*;
use process_support::reserve_tcp_address;
use wire_support::{empty_config, socket_bind};

const POOL: &str = "origin";

fn server(name: &str, address: SocketAddr) -> UpstreamServer {
    UpstreamServer {
        name: name.into(),
        endpoint: UpstreamEndpoint::Socket { address },
        max_connections: None,
        dns_resolution: DnsResolutionPolicy::OnConnect,
    }
}

fn ejecting_for_five_minutes() -> PassiveHealthPolicy {
    PassiveHealthPolicy {
        observe: PassiveObserve::Layer7,
        on_error: PassiveOnError::Immediately,
        error_limit: 1,
        mark_down: false,
        mark_up: false,
        initial_backoff_ms: 300_000,
        max_backoff_ms: 300_000,
        recovery_threshold: 1,
        readmission: None,
        circuit_breaker: None,
        outlier_detection: None,
    }
}

fn probing_every_half_minute() -> HealthCheck {
    HealthCheck {
        kind: HealthCheckType::Http,
        interval_ms: 30_000,
        timeout_ms: 2_000,
        healthy_threshold: 1,
        unhealthy_threshold: 1,
        startup: HealthStartup::Checking,
        fast_interval_ms: Some(30_000),
        down_interval_ms: Some(30_000),
        host: None,
        path: Some("/health".into()),
        expected_status: Some(200),
        http_version: None,
    }
}

fn config(
    management: SocketAddr,
    listener: SocketAddr,
    servers: Vec<UpstreamServer>,
    passive: Option<PassiveHealthPolicy>,
    check: Option<HealthCheck>,
) -> ConfigDraft {
    ConfigDraft {
        management: Some(super::management(management)),
        listeners: vec![Listener {
            name: "web".into(),
            bind: socket_bind(listener),
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(64),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        upstream_pools: vec![UpstreamPool {
            name: POOL.into(),
            servers,
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: check,
            passive_health: passive,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: Some(1_000),
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "web".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: POOL.into(),
                    policy: HttpProxyPolicy::default().into(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(1_024),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    }
}

async fn status_of(listener: SocketAddr) -> u16 {
    http_request(listener, "GET", "/", &[("Connection", "close")], &[])
        .await
        .status
}

async fn ejects_the_dead_server_and_survives_a_reload(supervised: bool) {
    let origin = spawn_origin();
    let (management, listener) = (reserve_tcp_address(), reserve_tcp_address());
    let dead = reserve_tcp_address();
    let mut draft = config(
        management,
        listener,
        vec![server("live", origin.address), server("dead", dead)],
        Some(ejecting_for_five_minutes()),
        None,
    );
    let mut server = start(&draft.clone().validate().unwrap(), supervised);
    server.wait_for_tcp(management).await;
    server.wait_for_tcp(listener).await;

    let ejected = wait_until("the dead server is ejected", || async {
        status_of(listener).await;
        let state = server_state(management, POOL, "dead").await;
        (state["passiveEjected"] == true).then_some(state)
    })
    .await;
    let until = ejected["passiveEjectionUntilUnixMs"].clone();
    assert_eq!(ejected["passiveEjectionCount"], "1");

    reload(&server, management, &mut draft, 1).await;

    let kept = server_state(management, POOL, "dead").await;
    assert_eq!(
        kept["passiveEjected"], true,
        "the reload ended the ejection"
    );
    assert_eq!(kept["passiveEjectionCount"], "1");
    assert_eq!(kept["passiveEjectionUntilUnixMs"], until);
    assert_eq!(kept["passiveFailureCount"], ejected["passiveFailureCount"]);
    for _ in 0..6 {
        assert_eq!(status_of(listener).await, 200);
    }
    let after = server_state(management, POOL, "dead").await;
    assert_eq!(
        after["passiveFailureCount"], ejected["passiveFailureCount"],
        "the replacement tried the ejected server again"
    );
    server.shutdown();
}

#[tokio::test]
async fn a_direct_reload_keeps_a_passive_ejection() {
    ejects_the_dead_server_and_survives_a_reload(false).await;
}

#[tokio::test]
async fn a_supervised_reload_keeps_a_passive_ejection() {
    ejects_the_dead_server_and_survives_a_reload(true).await;
}

async fn checks_each_server_once_across_a_reload(supervised: bool) {
    let origin = spawn_origin();
    let (management, listener) = (reserve_tcp_address(), reserve_tcp_address());
    let mut draft = config(
        management,
        listener,
        vec![server("live", origin.address)],
        None,
        Some(probing_every_half_minute()),
    );
    let mut server = start(&draft.clone().validate().unwrap(), supervised);
    server.wait_for_tcp(management).await;
    server.wait_for_tcp(listener).await;
    wait_until("the first health check", || async {
        let state = server_state(management, POOL, "live").await;
        (state["state"] == "healthy").then_some(())
    })
    .await;
    assert_eq!(origin.probes.load(Ordering::SeqCst), 1);

    reload(&server, management, &mut draft, 1).await;

    let state = server_state(management, POOL, "live").await;
    assert_eq!(
        state["state"], "healthy",
        "the reload forgot the health state"
    );
    sleep(Duration::from_millis(750)).await;
    assert_eq!(
        origin.probes.load(Ordering::SeqCst),
        1,
        "the replacement checked a server its predecessor had just checked"
    );
    assert_eq!(status_of(listener).await, 200);
    server.shutdown();
}

#[tokio::test]
async fn a_direct_reload_does_not_check_a_healthy_server_again() {
    checks_each_server_once_across_a_reload(false).await;
}

#[tokio::test]
async fn a_supervised_reload_does_not_check_a_healthy_server_again() {
    checks_each_server_once_across_a_reload(true).await;
}
