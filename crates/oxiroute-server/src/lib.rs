use std::net::SocketAddr;

use async_trait::async_trait;
use oxiroute_config::{Config, Protocol};
use pingora::{
    proxy::{ProxyHttp, Session},
    upstreams::peer::HttpPeer,
};

mod rtmp_api;

pub use rtmp_api::{ApiResponse, RtmpManagementApi};

pub struct HttpReverseProxy {
    upstream: SocketAddr,
}

impl HttpReverseProxy {
    #[must_use]
    pub fn new(upstream: SocketAddr) -> Self {
        Self { upstream }
    }
}

#[async_trait]
impl ProxyHttp for HttpReverseProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new(self.upstream, false, String::new())))
    }
}

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
