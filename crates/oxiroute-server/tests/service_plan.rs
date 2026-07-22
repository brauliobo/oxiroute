use std::{net::SocketAddr, sync::Arc, time::Duration};

use http::{Method, Uri, uri::Authority};
use oxiroute_config::{
    Config, HttpRoute, HttpService, L4Service, Listener, Protocol, UpstreamAlgorithm, UpstreamPool,
};
use oxiroute_server::{ServiceKind, ServicePlanError, service_specs};

#[test]
fn compiles_shared_http_and_l4_service_plans() {
    let config = canonical_config();

    let services = service_specs(&config).expect("valid service plan");

    assert_eq!(services.len(), 4);
    assert_eq!(services[0].name, "web");
    assert_eq!(services[0].bind, address(8080));
    assert_eq!(services[0].max_connections, 500);

    let ServiceKind::Http(first_http) = &services[0].kind else {
        panic!("first service must be HTTP");
    };
    let ServiceKind::Http(second_http) = &services[1].kind else {
        panic!("second service must be HTTP");
    };
    assert!(Arc::ptr_eq(first_http, second_http));
    assert_eq!(first_http.upstream_io_timeout(), Duration::from_secs(15));
    assert_eq!(first_http.max_request_body_bytes(), 2 * 1024 * 1024);
    assert_eq!(first_http.max_retries(), 1);
    let authority = "api.example.com".parse::<Authority>().expect("authority");
    let uri = "/v1/items".parse::<Uri>().expect("URI");
    assert_eq!(
        first_http.select(Some(&authority), &uri, &Method::GET),
        Some(address(3000))
    );
    assert_eq!(
        second_http.select(Some(&authority), &uri, &Method::GET),
        Some(address(3001))
    );

    let ServiceKind::Tcp(l4) = &services[2].kind else {
        panic!("third service must be TCP");
    };
    assert_eq!(l4.select(), address(5432));
    assert_eq!(l4.policy().connect, Duration::from_secs(5));
    assert_eq!(l4.policy().idle, Some(Duration::from_secs(120)));
    assert_eq!(l4.policy().lifetime, Some(Duration::from_secs(600)));
    assert!(matches!(services[3].kind, ServiceKind::Rtmp));
}

#[test]
fn rejects_an_invalid_programmatic_listener_without_panicking() {
    let mut config = canonical_config();
    config.listeners[0].service = None;

    assert!(matches!(
        service_specs(&config),
        Err(ServicePlanError::MissingListenerService { listener }) if listener == "web"
    ));
}

#[test]
fn rejects_an_invalid_programmatic_route_pool_reference() {
    let mut config = canonical_config();
    config.http_services[0].routes[0].upstream_pool = "missing".into();

    assert!(matches!(
        service_specs(&config),
        Err(ServicePlanError::UnknownHttpPool { service, route: 0, pool })
            if service == "api" && pool == "missing"
    ));
}

fn canonical_config() -> Config {
    Config {
        version: 1,
        management: None,
        listeners: vec![
            Listener {
                name: "web".into(),
                bind: address(8080),
                protocol: Protocol::Http,
                service: Some("api".into()),
                max_connections: 500,
            },
            Listener {
                name: "web-alt".into(),
                bind: address(8081),
                protocol: Protocol::Http,
                service: Some("api".into()),
                max_connections: 250,
            },
            Listener {
                name: "database".into(),
                bind: address(15432),
                protocol: Protocol::Tcp,
                service: Some("database".into()),
                max_connections: 100,
            },
            Listener {
                name: "live".into(),
                bind: address(1935),
                protocol: Protocol::Rtmp,
                service: None,
                max_connections: 50,
            },
        ],
        upstream_pools: vec![
            UpstreamPool {
                name: "api".into(),
                endpoints: vec![address(3000), address(3001)],
                algorithm: UpstreamAlgorithm::RoundRobin,
            },
            UpstreamPool {
                name: "database".into(),
                endpoints: vec![address(5432)],
                algorithm: UpstreamAlgorithm::RoundRobin,
            },
        ],
        http_services: vec![HttpService {
            name: "api".into(),
            routes: vec![HttpRoute {
                host: Some("api.example.com".into()),
                path_prefix: "/v1".into(),
                methods: vec!["GET".into()],
                upstream_pool: "api".into(),
            }],
            upstream_io_timeout_ms: 15_000,
            max_request_body_bytes: 2 * 1024 * 1024,
            max_retries: 1,
        }],
        l4_services: vec![L4Service {
            name: "database".into(),
            upstream_pool: "database".into(),
            connect_timeout_ms: 5_000,
            idle_timeout_ms: 120_000,
            lifetime_timeout_ms: Some(600_000),
        }],
    }
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}
