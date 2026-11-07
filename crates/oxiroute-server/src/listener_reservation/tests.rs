use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use oxiroute_config::{ConfigDraft, Protocol, ValidatedConfig};
use serde_json::json;

use super::*;
use crate::listener_inventory::{
    ListenerDescriptorKind, ListenerDescriptorRole, ListenerId, ListenerInventory,
    ListenerMetricPolicy, ListenerPlane,
};

mod descriptors;
mod inventory;
mod reuse;
mod unix;
mod validation;

fn config(name: &str, address: SocketAddr) -> ConfigDraft {
    ConfigDraft {
        listeners: vec![oxiroute_config::Listener {
            name: name.into(),
            bind: ListenerBind::Socket { address },
            protocol: oxiroute_config::Protocol::Rtmp,
            service: Some("rtmp".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
        ..empty_config()
    }
}

fn empty_config() -> ConfigDraft {
    ConfigDraft {
        version: 1,
        max_connections: None,
        management: None,
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

#[expect(
    clippy::too_many_lines,
    reason = "one fixture keeps the complete listener inventory order visible"
)]
fn differential_config() -> ValidatedConfig {
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "management": { "bind": "127.0.0.1:9900" },
        "stats": {
            "binds": ["127.0.0.1:8404", "127.0.0.1:8405"],
            "pages": [{
                "bind": "127.0.0.1:8406",
                "uri_prefix": "/stats",
                "refresh_ms": 1000,
                "admin": "disabled",
                "max_connections": 17
            }]
        },
        "certificates": [{
            "name": "downstream",
            "dns_names": ["proxy.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/tmp/oxiroute-listener-inventory-chain.pem",
                "private_key_path": "/tmp/oxiroute-listener-inventory-key.pem"
            }
        }],
        "tls_profiles": [{
            "name": "h3",
            "certificates": ["downstream"],
            "default_certificate": "downstream",
            "min_version": "1.3",
            "alpn": ["h3"]
        }],
        "listeners": [
            {
                "name": "http",
                "bind": { "type": "socket", "address": "127.0.0.1:7996" },
                "protocol": "http",
                "service": "web",
                "max_connections": 7
            },
            {
                "name": "rtmp",
                "bind": { "type": "socket", "address": "127.0.0.1:7997" },
                "protocol": "rtmp",
                "service": "live",
                "max_connections": 8
            },
            {
                "name": "forward-h1",
                "bind": { "type": "socket", "address": "127.0.0.1:7998" },
                "protocol": "forward_http1",
                "service": "forward",
                "max_connections": 9
            },
            {
                "name": "forward-h2",
                "bind": { "type": "socket", "address": "127.0.0.1:7999" },
                "protocol": "forward_http2",
                "service": "forward",
                "max_connections": 10
            },
            {
                "name": "tcp",
                "bind": { "type": "socket", "address": "127.0.0.1:8000" },
                "protocol": "tcp",
                "service": "relay",
                "max_connections": 11
            },
            {
                "name": "unix",
                "bind": { "type": "unix", "path": "/tmp/inventory.sock", "mode": 384 },
                "protocol": "tcp",
                "service": "relay"
            },
            {
                "name": "udp",
                "bind": { "type": "udp", "address": "127.0.0.1:8001" },
                "protocol": "udp",
                "service": "relay"
            },
            {
                "name": "h3",
                "bind": { "type": "udp", "address": "127.0.0.1:8002" },
                "protocol": "http3",
                "service": "web",
                "tls_profile": "h3"
            },
            {
                "name": "forward-h3",
                "bind": { "type": "udp", "address": "127.0.0.1:8003" },
                "protocol": "forward_http3",
                "service": "forward",
                "tls_profile": "h3"
            }
        ],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": { "kind": "segment_prefix", "value": "/" },
                "policy": { "request_buffering": true },
                "action": { "type": "fixed_response", "status": 200, "body": "ok" }
            }],
            "max_request_body_bytes": 65536
        }],
        "forward_proxy_services": [{
            "name": "forward",
            "enabled_versions": ["h1", "h2", "h3"],
            "tls_required": false
        }],
        "rtmp_services": [{
            "name": "live",
            "applications": [{ "name": "broadcast", "live": true }]
        }],
        "upstream_pools": [{
            "name": "origin",
            "endpoints": [{ "type": "socket", "address": "127.0.0.1:9000" }],
            "algorithm": "round_robin"
        }],
        "l4_services": [{
            "name": "relay",
            "upstream_pool": "origin",
            "udp": {}
        }]
    }))
    .expect("inventory draft")
    .validate()
    .expect("valid inventory config")
}

fn many_listener_config(count: usize) -> ValidatedConfig {
    let mut config = empty_config();
    config.upstream_pools.push(oxiroute_config::UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![oxiroute_config::UpstreamEndpoint::Socket {
            address: "127.0.0.1:9000".parse().expect("upstream address"),
        }],
        algorithm: oxiroute_config::UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: oxiroute_config::HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        resilience: oxiroute_config::PoolResilience::default(),
    });
    config.l4_services.push(oxiroute_config::L4Service {
        name: "relay".into(),
        upstream_pool: "origin".into(),
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 1_000,
        lifetime_timeout_ms: None,
        proxy_protocol: None,
        udp: None,
    });
    config.listeners = (0..count)
        .map(|index| oxiroute_config::Listener {
            name: format!("listener-{index}"),
            bind: ListenerBind::Socket {
                address: SocketAddr::from((
                    [127, 0, 0, 1],
                    10_000 + u16::try_from(index).expect("bounded listener index"),
                )),
            },
            protocol: Protocol::Tcp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        })
        .collect();
    config.validate().expect("valid many-listener config")
}

fn prepare_reservations(
    config: &ConfigDraft,
    previous: Option<&ListenerReservations>,
) -> io::Result<ListenerReservations> {
    let config = validated_test_config(config)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    ListenerReservations::prepare(&config, previous)
}

#[cfg(target_os = "linux")]
fn adopt_reservations(
    config: &ConfigDraft,
    descriptors: DescriptorSet,
) -> io::Result<ListenerReservations> {
    let config = validated_test_config(config)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    ListenerReservations::adopt(&config, descriptors)
}

#[cfg(target_os = "linux")]
fn export_reservations(
    reservations: &ListenerReservations,
    config: &ConfigDraft,
) -> io::Result<(DescriptorManifest, Vec<std::os::fd::OwnedFd>)> {
    let config = validated_test_config(config)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    reservations.export_descriptors(&ListenerInventory::compile(&config))
}

fn validated_test_config(
    config: &ConfigDraft,
) -> Result<ValidatedConfig, oxiroute_config::ConfigError> {
    let mut complete = differential_config().to_draft();
    complete.listeners = config.listeners.clone();
    complete.management.clone_from(&config.management);
    complete.stats.clone_from(&config.stats);
    for listener in &mut complete.listeners {
        match listener.protocol {
            Protocol::Http | Protocol::Http3 => listener.service = Some("web".into()),
            Protocol::Rtmp => listener.service = Some("live".into()),
            Protocol::Tcp | Protocol::Udp => listener.service = Some("relay".into()),
            Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3 => {
                listener.service = Some("forward".into());
            }
        }
        if matches!(listener.protocol, Protocol::Http3 | Protocol::ForwardHttp3) {
            listener.tls_profile = Some("h3".into());
        }
    }
    complete.validate()
}
