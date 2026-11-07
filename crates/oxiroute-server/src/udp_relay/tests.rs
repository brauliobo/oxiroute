use std::net::{SocketAddr, UdpSocket as StdUdpSocket};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, L4Service, Listener, ListenerBind, PassiveHealthPolicy,
    PassiveObserve, PassiveOnError, Protocol, ProxyProtocolPolicy, ProxyProtocolVersion,
    UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
};
use oxiroute_config_source::ConfigFormat;

use crate::{
    GenerationManager, RuntimeReferenceKind, ServiceKind,
    config_coordinator::{AuthoredRevision, EffectiveRevision, ResolvedConfigDocument},
};

use super::*;

fn policy() -> UdpPolicy {
    UdpPolicy {
        max_datagram_bytes: 8,
        max_sessions: 2,
        max_session_bytes: 64,
        max_queue_datagrams: 2,
        max_queue_bytes: 12,
    }
}

mod lifecycle;
mod passive_health;
mod proxy_protocol;
mod queue;
mod sessions;

#[cfg(unix)]
struct UdpHarness {
    listener: SocketAddr,
    upstream: UdpSocket,
    generation: Arc<RuntimeGeneration>,
    shutdown: watch::Sender<bool>,
    runtime: UdpRuntime,
}

#[cfg(unix)]
impl UdpHarness {
    fn stop(self) {
        self.shutdown.send(true).expect("shutdown UDP runtime");
        self.runtime.join().expect("join UDP runtime");
    }
}

#[cfg(unix)]
fn passive_health(error_limit: u16, backoff_ms: u64) -> PassiveHealthPolicy {
    PassiveHealthPolicy {
        observe: PassiveObserve::Layer7,
        on_error: PassiveOnError::Count,
        error_limit,
        mark_down: false,
        mark_up: false,
        initial_backoff_ms: backoff_ms,
        max_backoff_ms: backoff_ms,
        recovery_threshold: 1,
    }
}

#[cfg(unix)]
async fn start_harness(
    policy: UdpPolicy,
    idle_timeout: Duration,
    lifetime_timeout: Option<Duration>,
    max_connections: u64,
    proxy_protocol: Option<ProxyProtocolPolicy>,
) -> UdpHarness {
    start_harness_with_options(
        policy,
        idle_timeout,
        lifetime_timeout,
        max_connections,
        proxy_protocol,
        None,
        None,
        None,
    )
    .await
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn start_harness_with_options(
    policy: UdpPolicy,
    idle_timeout: Duration,
    lifetime_timeout: Option<Duration>,
    max_connections: u64,
    proxy_protocol: Option<ProxyProtocolPolicy>,
    passive_health: Option<PassiveHealthPolicy>,
    configured_upstream: Option<SocketAddr>,
    upstream_proxy_protocol: Option<ProxyProtocolPolicy>,
) -> UdpHarness {
    let upstream = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("upstream socket");
    let upstream_address =
        configured_upstream.unwrap_or_else(|| upstream.local_addr().expect("upstream address"));
    let listener = StdUdpSocket::bind(("127.0.0.1", 0))
        .expect("listener probe")
        .local_addr()
        .expect("listener address");
    let manager = GenerationManager::new();
    let mut config = udp_config(listener, upstream_address);
    config.listeners[0].max_connections = Some(max_connections);
    config.listeners[0].proxy_protocol = proxy_protocol;
    config.l4_services[0].idle_timeout_ms =
        u64::try_from(idle_timeout.as_millis()).expect("test idle timeout fits");
    config.l4_services[0].lifetime_timeout_ms =
        lifetime_timeout.map(|value| u64::try_from(value.as_millis()).expect("test lifetime fits"));
    config.l4_services[0].proxy_protocol = upstream_proxy_protocol;
    config.l4_services[0].udp = Some(policy);
    config.upstream_pools[0].passive_health = passive_health;
    let candidate = manager
        .prepare(document(config))
        .expect("prepare UDP generation");
    let generation = manager
        .activate(&candidate)
        .expect("activate UDP generation");
    generation
        .metrics()
        .register_configured_listener(
            "relay",
            "udp",
            &ListenerBind::Udp { address: listener },
            Some(max_connections),
        )
        .expect("register UDP listener metrics");
    let service = match &generation.services()[0].kind {
        ServiceKind::Udp(service) => Arc::clone(service),
        _ => panic!("test service must be UDP"),
    };
    let reservation = generation
        .reservations()
        .get("relay")
        .cloned()
        .expect("UDP reservation");
    let metrics = generation
        .metrics()
        .listener("relay")
        .expect("listener metrics registry")
        .expect("UDP listener metrics");
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let runtime = UdpRuntime::start(
        "relay".into(),
        reservation,
        service,
        Arc::clone(&generation),
        metrics,
        proxy_protocol,
        shutdown_receiver,
    )
    .expect("start UDP runtime");
    UdpHarness {
        listener,
        upstream,
        generation,
        shutdown,
        runtime,
    }
}

#[cfg(unix)]
async fn wait_for_udp_references(generation: &RuntimeGeneration, expected: u64) {
    timeout(Duration::from_secs(2), async {
        loop {
            if generation.active_references(RuntimeReferenceKind::Udp) == expected {
                break;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("UDP generation reference timeout");
}

#[cfg(unix)]
async fn wait_for_udp_passive_failures(generation: &RuntimeGeneration, expected: u64) {
    timeout(Duration::from_secs(2), async {
        loop {
            let failures =
                generation.pools()[0].health_snapshot().endpoints[0].passive_failure_count;
            if failures >= expected {
                break;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("UDP passive failure timeout");
}

#[cfg(unix)]
fn udp_passive_failure_count(generation: &RuntimeGeneration) -> u64 {
    generation.pools()[0].health_snapshot().endpoints[0].passive_failure_count
}

#[cfg(unix)]
fn udp_config(listener: SocketAddr, upstream: SocketAddr) -> ConfigDraft {
    ConfigDraft {
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: vec![Listener {
            name: "relay".into(),
            bind: ListenerBind::Udp { address: listener },
            protocol: Protocol::Udp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        cache_stores: Vec::new(),
        upstream_pools: vec![UpstreamPool {
            name: "upstream".into(),
            servers: Vec::new(),
            endpoints: vec![UpstreamEndpoint::Socket { address: upstream }],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: oxiroute_config::HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            resilience: oxiroute_config::PoolResilience::default(),
        }],
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: vec![L4Service {
            drain_deadline_ms: None,
            name: "relay".into(),
            upstream_pool: "upstream".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 10_000,
            lifetime_timeout_ms: Some(30_000),
            proxy_protocol: None,
            udp: Some(policy()),
        }],
    }
}

fn document(config: ConfigDraft) -> ResolvedConfigDocument {
    ResolvedConfigDocument {
        authored_revision: AuthoredRevision::from_bytes(b"udp-test"),
        effective_revision: EffectiveRevision::from_bytes(b"udp-test"),
        validated_config: config.validate().expect("valid UDP test config"),
        format: ConfigFormat::Lua,
        compositional: false,
        dependencies: Vec::new(),
        config_preview: String::new(),
        diagnostics: Vec::new(),
    }
}
