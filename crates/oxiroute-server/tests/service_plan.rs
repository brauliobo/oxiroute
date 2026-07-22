use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use oxiroute_config::{Config, ConfigError, Listener, Protocol};
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
                upstream: Some(address(3000)),
            },
            Listener {
                name: "database".into(),
                bind: address(5432),
                protocol: Protocol::Tcp,
                upstream: Some(address(15432)),
            },
            Listener {
                name: "live".into(),
                bind: address(1935),
                protocol: Protocol::Rtmp,
                upstream: None,
            },
        ],
    };

    let services = service_specs(&config).expect("valid service plan");

    assert_eq!(services.len(), 3);
    assert_eq!(services[0].name, "web");
    assert_eq!(services[0].bind, address(8080));
    assert_eq!(services[0].kind, ServiceKind::Http(address(3000)));
    assert_eq!(services[1].kind, ServiceKind::Tcp(address(15432)));
    assert_eq!(services[2].kind, ServiceKind::Rtmp);
}

#[test]
fn rejects_an_invalid_programmatic_listener_without_panicking() {
    let config = Config {
        version: 1,
        management: None,
        listeners: vec![Listener {
            name: "web".into(),
            bind: address(8080),
            protocol: Protocol::Http,
            upstream: None,
        }],
    };

    assert!(matches!(
        service_specs(&config),
        Err(ConfigError::MissingUpstream { listener, protocol: Protocol::Http })
            if listener == "web"
    ));
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}
