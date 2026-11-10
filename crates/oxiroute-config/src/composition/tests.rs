use std::net::{Ipv4Addr, SocketAddr};

use crate::{
    DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpVersionPolicy, L4Service, Listener,
    ListenerBind, Protocol, StatsPage, StatsPageAdminPolicy, UpstreamAlgorithm,
    UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};

use super::*;

#[test]
fn composes_namespaces_in_input_order() {
    let first = tcp_config("nginx", 80, 9080);
    let mut second = tcp_config("haproxy", 8080, 9081);
    second.max_connections = Some(4096);

    let composed = compose_validated_configs(vec![first, second]).expect("composed config");
    let composed = composed.as_draft();

    assert_eq!(composed.max_connections, Some(4096));
    assert_eq!(composed.listeners[0].name, "nginx");
    assert_eq!(composed.listeners[1].name, "haproxy");
}

#[test]
fn composes_independent_stats_binds_and_pages() {
    let mut first = tcp_config("nginx", 80, 9080);
    first.stats = Some(Stats {
        binds: vec!["127.0.0.1:9000".parse().expect("stats bind")],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let mut second = tcp_config("haproxy", 8080, 9081);
    second.stats = Some(Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: "127.0.0.1:9001".parse().expect("stats page bind"),
            uri_prefix: "/stats".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
    });

    let composed = compose_validated_configs(vec![first, second]).expect("composed stats");
    let composed = composed.as_draft();
    let stats = composed.stats.as_ref().expect("stats process");
    assert_eq!(stats.binds.len(), 1);
    assert_eq!(stats.pages.len(), 1);
}

#[test]
fn rejects_conflicting_process_fields() {
    let mut first = empty_config();
    first.max_connections = Some(1024);
    let mut second = empty_config();
    second.max_connections = Some(4096);

    assert!(matches!(
        compose_validated_configs(vec![first, second]),
        Err(ConfigCompositionError::ProcessFieldConflict {
            field: "max_connections"
        })
    ));
}

#[test]
fn validates_the_composed_namespace() {
    let first = tcp_config("shared", 80, 9080);
    let second = tcp_config("shared", 8080, 9081);

    assert!(matches!(
        compose_validated_configs(vec![first, second]),
        Err(ConfigCompositionError::Invalid(ConfigError::DuplicateName {
            namespace: "listener",
            name
        })) if name == "shared"
    ));
}

#[test]
fn validates_only_after_complete_drafts_are_composed() {
    let mut listener = tcp_config("edge", 80, 9080);
    let mut services = empty_config();
    services.upstream_pools = std::mem::take(&mut listener.upstream_pools);
    services.l4_services = std::mem::take(&mut listener.l4_services);

    assert!(listener.clone().validate().is_err());
    assert!(services.clone().validate().is_ok());

    let composed =
        compose_validated_configs(vec![listener, services]).expect("complete composition");
    let composed = composed.as_draft();
    assert_eq!(composed.listeners.len(), 1);
    assert_eq!(composed.upstream_pools.len(), 1);
    assert_eq!(composed.l4_services.len(), 1);
}

fn empty_config() -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

fn tcp_config(name: &str, port: u16, upstream_port: u16) -> ConfigDraft {
    let mut config = empty_config();
    let pool = format!("{name}-pool");
    let service = format!("{name}-service");
    config.upstream_pools.push(UpstreamPool {
        name: pool.clone(),
        servers: vec![UpstreamServer {
            name: format!("{name}-server"),
            endpoint: UpstreamEndpoint::Socket {
                address: SocketAddr::from((Ipv4Addr::LOCALHOST, upstream_port)),
            },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::default(),
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::default(),
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: crate::PoolResilience::default(),
        queue: None,
    });
    config.l4_services.push(L4Service {
        drain_deadline_ms: None,
        name: service.clone(),
        upstream_pool: pool,
        connect_timeout_ms: 5000,
        idle_timeout_ms: 60_000,
        lifetime_timeout_ms: None,
        proxy_protocol: None,
        udp: None,
    });
    config.listeners.push(Listener {
        name: name.into(),
        bind: ListenerBind::Socket {
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        },
        protocol: Protocol::Tcp,
        service: Some(service),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });
    config
}
