use std::net::SocketAddr;

use async_trait::async_trait;
use oxiroute_config::{Config, ConfigError, Protocol};
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
    pub kind: ServiceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    Http(SocketAddr),
    Rtmp,
    Tcp(SocketAddr),
}

/// Compiles validated listener definitions into runtime service specifications.
///
/// # Errors
///
/// Returns an error when a programmatically constructed configuration has an upstream that does
/// not match its listener protocol.
pub fn service_specs(config: &Config) -> Result<Vec<ServiceSpec>, ConfigError> {
    config
        .listeners
        .iter()
        .map(|listener| {
            let kind = match (listener.protocol, listener.upstream) {
                (Protocol::Http, Some(upstream)) => ServiceKind::Http(upstream),
                (Protocol::Tcp, Some(upstream)) => ServiceKind::Tcp(upstream),
                (Protocol::Rtmp, None) => ServiceKind::Rtmp,
                (Protocol::Http | Protocol::Tcp, None) => {
                    return Err(ConfigError::MissingUpstream {
                        listener: listener.name.clone(),
                        protocol: listener.protocol,
                    });
                }
                (Protocol::Rtmp, Some(_)) => {
                    return Err(ConfigError::UnexpectedRtmpUpstream(listener.name.clone()));
                }
            };
            Ok(ServiceSpec {
                name: listener.name.clone(),
                bind: listener.bind,
                kind,
            })
        })
        .collect()
}
