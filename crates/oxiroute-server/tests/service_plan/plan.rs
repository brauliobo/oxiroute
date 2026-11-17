use std::{sync::Arc, time::Duration};

use http::{Method, Uri, uri::Authority};
use oxiroute_config::{
    ConfigError, HttpVersionPolicy, L4Service, Listener, ListenerBind, Protocol, UdpPolicy,
    UpstreamAlgorithm, UpstreamPool,
};
use oxiroute_server::{RuntimeEndpoint, ServiceKind, ServicePlanError};

use crate::canonical::canonical_config;
use crate::config_support::{
    self, load_lua, loopback_address as address, loopback_bind as socket_bind,
    loopback_endpoint as socket_endpoint, runtime_plan, service_specs,
};

#[test]
fn distributed_example_compiles_into_an_active_runtime_plan() {
    let config = load_lua(include_str!("../../../../oxiroute.example.lua"))
        .expect("distributed example configuration");

    runtime_plan(&config).expect("distributed example runtime plan");
    let mut draft = config.to_draft();
    draft.management = None;
    draft.stats = None;
    for listener in &mut draft.listeners {
        if let ListenerBind::Socket { address, .. } = &mut listener.bind {
            let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            *address = reservation.local_addr().unwrap();
        }
    }
    let generation =
        config_support::runtime_generation(&draft.validate().unwrap()).expect("runtime generation");

    assert_eq!(generation.services().len(), 3);
    assert!(generation.health_supervisor().is_some());
}

#[test]
fn bounded_buffering_policies_compile_and_unbounded_buffering_fails_closed() {
    let mut config = canonical_config();
    config.http_services[0].routes[0].policy.request_buffering = true;

    runtime_plan(&config.clone().validate().unwrap())
        .expect("request buffering has an active runtime");

    config.http_services[0].routes[0].policy.response_buffering = true;

    runtime_plan(&config.clone().validate().unwrap())
        .expect("bounded response buffering has an active runtime");

    config.http_services[0].routes[0]
        .policy
        .max_request_body_bytes = None;
    assert!(matches!(
        config.clone().validate(),
        Err(ConfigError::InvalidHttpRoute { .. })
    ));

    config.http_services[0].routes[0].policy.response_buffering = false;
    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::RuntimePolicyUnavailable {
            policy: "http_services[].routes[].policy.unbounded_request_buffering"
        })
    ));
}

#[test]
fn runtime_rejects_duplicate_routes_until_importer_first_wins_has_resolved_them() {
    let mut config = canonical_config();
    let duplicate = config.http_services[0].routes[0].clone();
    config.http_services[0].routes.push(duplicate);

    assert!(matches!(
        config.clone().validate(),
        Err(ConfigError::DuplicateHttpRoute { .. })
    ));

    config.http_services[0].routes.pop();
    runtime_plan(&config.clone().validate().unwrap()).expect("importer-resolved first route only");
}

#[test]
fn compiles_shared_http_and_l4_service_plans() {
    let config = canonical_config();

    let services = service_specs(&config.clone().validate().unwrap()).expect("valid service plan");

    assert_eq!(services.len(), 4);
    assert_eq!(services[0].name, "web");
    assert_eq!(services[0].bind, socket_bind(8080));
    assert_eq!(services[0].max_connections, Some(500));
    assert!(services.iter().all(|service| service.tls.is_none()));

    let ServiceKind::Http(first_http) = &services[0].kind else {
        panic!("first service must be HTTP");
    };
    let ServiceKind::Http(second_http) = &services[1].kind else {
        panic!("second service must be HTTP");
    };
    assert!(Arc::ptr_eq(first_http, second_http));
    assert_eq!(first_http.upstream_io_timeout(), Duration::from_secs(15));
    assert_eq!(first_http.max_request_body_bytes(), Some(2 * 1024 * 1024));
    let authority = "api.example.com".parse::<Authority>().expect("authority");
    let uri = "/v1/items".parse::<Uri>().expect("URI");
    assert_eq!(
        first_http
            .select(Some(&authority), &uri, &Method::GET)
            .map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(address(3000)))
    );
    assert_eq!(
        second_http
            .select(Some(&authority), &uri, &Method::GET)
            .map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(address(3001)))
    );

    let ServiceKind::Tcp(l4) = &services[2].kind else {
        panic!("third service must be TCP");
    };
    assert_eq!(
        l4.select().map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(address(5432)))
    );
    assert_eq!(l4.policy().connect, Duration::from_secs(5));
    assert_eq!(l4.policy().idle, Some(Duration::from_mins(2)));
    assert_eq!(l4.policy().lifetime, Some(Duration::from_mins(10)));
    assert!(matches!(services[3].kind, ServiceKind::Rtmp(_)));
}

#[test]
fn compiles_a_bounded_udp_service_plan() {
    let mut config = canonical_config();
    config.upstream_pools.push(UpstreamPool {
        name: "dns".into(),
        servers: Vec::new(),
        endpoints: vec![socket_endpoint(5353)],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });
    config.l4_services.push(L4Service {
        drain_deadline_ms: None,
        name: "dns".into(),
        upstream_pool: "dns".into(),
        connect_timeout_ms: 2_000,
        idle_timeout_ms: 30_000,
        lifetime_timeout_ms: Some(120_000),
        proxy_protocol: None,
        udp: Some(UdpPolicy {
            max_datagram_bytes: 1_232,
            max_sessions: 32,
            max_session_bytes: 256 * 1_024,
            max_queue_datagrams: 8,
            max_queue_bytes: 16 * 1_024,
        }),
    });
    config.listeners.push(Listener {
        name: "dns-relay".into(),
        bind: ListenerBind::Udp {
            address: address(15353),
        },
        protocol: Protocol::Udp,
        service: Some("dns".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(32),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });

    let services =
        service_specs(&config.clone().validate().unwrap()).expect("bounded UDP service plan");
    let ServiceKind::Udp(udp) = &services[4].kind else {
        panic!("fifth service must be UDP");
    };
    assert_eq!(services[4].kind.protocol(), "udp");
    assert_eq!(udp.policy().connect, Duration::from_secs(2));
    assert_eq!(udp.policy().idle, Some(Duration::from_secs(30)));
    assert_eq!(udp.policy().lifetime, Some(Duration::from_mins(2)));
    assert_eq!(
        udp.udp_policy(),
        UdpPolicy {
            max_datagram_bytes: 1_232,
            max_sessions: 32,
            max_session_bytes: 256 * 1_024,
            max_queue_datagrams: 8,
            max_queue_bytes: 16 * 1_024,
        }
    );
}

#[test]
fn preserves_unbounded_listener_and_request_body_limits() {
    let mut config = canonical_config();
    config.listeners[0].max_connections = None;
    config.http_services[0].max_request_body_bytes = None;

    let services = service_specs(&config.validate().unwrap()).expect("unbounded service plan");
    assert_eq!(services[0].max_connections, None);
    let ServiceKind::Http(service) = &services[0].kind else {
        panic!("first service must be HTTP");
    };
    assert_eq!(service.max_request_body_bytes(), None);
}
