use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use oxiroute_config::{Config, Listener, Protocol};
use oxiroute_server::{service_specs, ServiceKind};

#[test]
fn plans_one_runtime_service_for_each_configured_listener() {
    let config = Config {
        version: 1,
        management: None,
        listeners: vec![
            Listener {
                name: "web".into(),
                bind: address(8080),
                protocol: Protocol::Http,
                upstream: address(3000),
            },
            Listener {
                name: "database".into(),
                bind: address(5432),
                protocol: Protocol::Tcp,
                upstream: address(15432),
            },
        ],
    };

    let services = service_specs(&config);

    assert_eq!(services.len(), 2);
    assert_eq!(services[0].name, "web");
    assert_eq!(services[0].bind, address(8080));
    assert_eq!(services[0].upstream, address(3000));
    assert_eq!(services[0].kind, ServiceKind::Http);
    assert_eq!(services[1].kind, ServiceKind::Tcp);
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}
