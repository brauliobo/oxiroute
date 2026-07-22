use std::{net::SocketAddr, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use log::warn;
use oxiroute_config::{Config, ConfigError, Protocol};
use pingora::{
    proxy::{ProxyHttp, Session},
    upstreams::peer::HttpPeer,
};

mod monitoring;
mod rtmp_api;

pub use monitoring::{
    ConnectionGuard, HostSnapshot, ListenerMetrics, ListenerSnapshot, MetricsError,
    ProcessSnapshot, RuntimeMetrics, RuntimeSnapshot, TrafficSnapshot,
};
pub use rtmp_api::{ApiResponse, RtmpManagementApi};

pub struct HttpReverseProxy {
    upstream: SocketAddr,
    metrics: ListenerMetrics,
}

impl HttpReverseProxy {
    #[must_use]
    pub fn new(upstream: SocketAddr, metrics: ListenerMetrics) -> Self {
        Self { upstream, metrics }
    }
}

pub struct HttpRequestMetrics {
    listener: ListenerMetrics,
    observed_received: u64,
    observed_sent: u64,
}

impl HttpRequestMetrics {
    fn observe(&mut self, session: &Session) {
        observe_counter(
            &self.listener,
            session.body_bytes_read(),
            &mut self.observed_received,
            true,
        );
        observe_counter(
            &self.listener,
            session.body_bytes_sent(),
            &mut self.observed_sent,
            false,
        );
    }
}

#[async_trait]
impl ProxyHttp for HttpReverseProxy {
    type CTX = HttpRequestMetrics;

    fn new_ctx(&self) -> Self::CTX {
        HttpRequestMetrics {
            listener: self.metrics.clone(),
            observed_received: 0,
            observed_sent: 0,
        }
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new(self.upstream, false, String::new())))
    }

    async fn request_body_filter(
        &self,
        session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        ctx.observe(session);
        Ok(())
    }

    fn response_body_filter(
        &self,
        session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Option<Duration>> {
        ctx.observe(session);
        Ok(None)
    }

    async fn logging(
        &self,
        session: &mut Session,
        _error: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        ctx.observe(session);
    }
}

fn observe_counter(listener: &ListenerMetrics, current: usize, observed: &mut u64, received: bool) {
    let Ok(current) = u64::try_from(current) else {
        warn!("HTTP byte counter exceeds the supported range");
        return;
    };
    let Some(delta) = current.checked_sub(*observed) else {
        warn!("HTTP byte counter moved backwards");
        return;
    };
    if delta == 0 {
        return;
    }
    let result = if received {
        listener.record_bytes_received(delta)
    } else {
        listener.record_bytes_sent(delta)
    };
    match result {
        Ok(()) => *observed = current,
        Err(error) => warn!("could not account for HTTP traffic: {error}"),
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

impl ServiceKind {
    #[must_use]
    pub const fn protocol(self) -> &'static str {
        match self {
            Self::Http(_) => "http",
            Self::Rtmp => "rtmp",
            Self::Tcp(_) => "tcp",
        }
    }
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
