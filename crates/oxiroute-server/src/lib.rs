use std::net::SocketAddr;

use oxiroute_config::{Config, Protocol};

#[derive(Debug, PartialEq, Eq)]
pub struct ServiceSpec {
    pub name: String,
    pub bind: SocketAddr,
    pub upstream: SocketAddr,
    pub kind: ServiceKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceKind {
    Http,
    Tcp,
}

#[must_use]
pub fn service_specs(config: &Config) -> Vec<ServiceSpec> {
    config
        .listeners
        .iter()
        .map(|listener| ServiceSpec {
            name: listener.name.clone(),
            bind: listener.bind,
            upstream: listener.upstream,
            kind: match listener.protocol {
                Protocol::Http => ServiceKind::Http,
                Protocol::Tcp => ServiceKind::Tcp,
            },
        })
        .collect()
}
