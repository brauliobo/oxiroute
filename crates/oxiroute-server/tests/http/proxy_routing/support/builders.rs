use crate::config_support::socket_endpoint;
use oxiroute_config::CacheStore;
use oxiroute_config::DnsResolutionPolicy;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpProxyPolicy;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::HttpVersionPolicy;
use oxiroute_config::UpstreamAlgorithm;
use oxiroute_config::UpstreamEndpoint;
use oxiroute_config::UpstreamPool;
use oxiroute_config::UpstreamServer;
use std::net::SocketAddr;
use std::path::PathBuf;

pub(crate) fn proxy_policy_mut(route: &mut HttpRoute) -> &mut HttpProxyPolicy {
    let HttpRouteAction::Proxy { policy, .. } = &mut route.action else {
        panic!("proxy route");
    };
    policy
}

pub(crate) fn pool(name: &str, endpoints: &[SocketAddr]) -> UpstreamPool {
    endpoint_pool(
        name,
        endpoints.iter().copied().map(socket_endpoint).collect(),
        UpstreamAlgorithm::RoundRobin,
    )
}

pub(crate) fn capped_pool(name: &str, address: SocketAddr, max_connections: u64) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: vec![UpstreamServer {
            name: "origin".into(),
            endpoint: socket_endpoint(address),
            max_connections: Some(max_connections),
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: Some(1_000),
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }
}

pub(crate) fn endpoint_pool(
    name: &str,
    endpoints: Vec<UpstreamEndpoint>,
    algorithm: UpstreamAlgorithm,
) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: Vec::new(),
        endpoints,
        algorithm,
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
    }
}

pub(crate) fn memory_cache_store(name: &str) -> CacheStore {
    CacheStore::Memory {
        name: name.into(),
        max_bytes: 1024 * 1024,
        max_entries: 128,
        max_object_bytes: 64 * 1024,
        max_header_bytes: 8 * 1024,
        max_key_bytes: 4 * 1024,
        max_tag_bytes: 256,
        max_tags_per_object: 64,
        max_in_flight_fills: 16,
        max_followers_per_fill: 16,
    }
}

pub(crate) fn disk_cache_store(name: &str, root_directory: PathBuf) -> CacheStore {
    CacheStore::Disk {
        name: name.into(),
        root_directory,
        max_bytes: 1024 * 1024,
        max_files: 128,
        max_object_bytes: 64 * 1024,
        max_header_bytes: 8 * 1024,
        max_key_bytes: 4 * 1024,
        max_tag_bytes: 256,
        max_tags_per_object: 64,
        max_in_flight_fills: 16,
        max_followers_per_fill: 16,
    }
}

pub(crate) fn route(host: Option<&str>, path: &str, methods: &[&str], pool: &str) -> HttpRoute {
    HttpRoute {
        host: host.map(|value| oxiroute_config::HttpHostSelector::NormalizedHost {
            value: value.into(),
        }),
        path: HttpPathSelector::SegmentPrefix { value: path.into() },
        methods: methods.iter().map(ToString::to_string).collect(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::Proxy {
            upstream_pool: pool.into(),
            policy: HttpProxyPolicy::default().into(),
        },
    }
}
