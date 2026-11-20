use std::sync::Arc;

use pingora::protocols::{ALPN, SocketDigest, Stream, tls::SslDigest};

use super::{ConnStats, Connection, Protocol, TlsInfo};
use crate::ListenerMetrics;

/// Who counts the bytes and the completions of a connection's requests.
#[derive(Clone)]
pub(crate) enum Counter {
    /// The request path of the tokio data plane does.
    Request(Arc<ConnStats>),
    /// The epoll engine does, so the request path only identifies the connection.
    Engine(Arc<ConnStats>),
}

impl Counter {
    pub(crate) const fn stats(&self) -> &Arc<ConnStats> {
        match self {
            Self::Request(stats) | Self::Engine(stats) => stats,
        }
    }

    pub(crate) const fn credited(&self) -> Option<&Arc<ConnStats>> {
        match self {
            Self::Request(stats) => Some(stats),
            Self::Engine(_) => None,
        }
    }
}

pub(crate) fn counter_of(digest: Option<&SocketDigest>) -> Option<Counter> {
    let connection = digest?.tracking()?.downcast_ref::<Connection>()?;
    Some(Counter::Request(Arc::clone(connection.stats())))
}

pub(crate) fn of_stream(stream: &Stream) -> Option<Counter> {
    counter_of(stream.get_socket_digest().as_deref())
}

fn tls_version(digest: &SslDigest) -> &'static str {
    if digest.version.contains("1.3") {
        "TLS1.3"
    } else if digest.version.contains("1.2") {
        "TLS1.2"
    } else {
        "TLS"
    }
}

fn alpn_name(alpn: Option<ALPN>) -> Option<String> {
    match alpn? {
        ALPN::H1 => Some("http/1.1".to_owned()),
        ALPN::H2 => Some("h2".to_owned()),
        ALPN::H2H1 | ALPN::Custom(_) => None,
    }
}

/// Registers the connection of `stream` the first time one of its rounds is served; later rounds find it.
pub(crate) fn track(metrics: &ListenerMetrics, stream: &Stream) {
    let Some(digest) = stream.get_socket_digest() else {
        return;
    };
    digest.tracking_or_init(|| {
        let peer = *digest.peer_addr()?.as_inet()?;
        let alpn = stream.selected_alpn_proto();
        let protocol = if alpn == Some(ALPN::H2) {
            Protocol::H2
        } else {
            Protocol::Http1
        };
        let connection = metrics.track(peer, protocol)?;
        if let Some(ssl) = stream.get_ssl_digest() {
            connection.stats().set_tls(TlsInfo {
                version: tls_version(&ssl),
                sni: None,
                alpn: alpn_name(alpn),
            });
        }
        Some(Arc::new(connection))
    });
}
