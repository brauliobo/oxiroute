use std::{sync::Arc, time::Instant};

use quinn::crypto::rustls::HandshakeData;

use crate::{
    ListenerMetrics,
    clients::{Concurrent, ConnStats, TlsInfo},
};

#[derive(Clone)]
pub(crate) struct Traffic {
    metrics: ListenerMetrics,
    tracked: Option<Arc<ConnStats>>,
}

impl Traffic {
    pub(crate) const fn new(metrics: ListenerMetrics, tracked: Option<Arc<ConnStats>>) -> Self {
        Self { metrics, tracked }
    }

    pub(crate) const fn metrics(&self) -> &ListenerMetrics {
        &self.metrics
    }

    pub(crate) fn received(&self, bytes: u64) {
        let _ = self.metrics.record_bytes_received(bytes);
        if let Some(stats) = &self.tracked {
            stats.received::<Concurrent>(bytes, Instant::now());
        }
    }

    pub(crate) fn sent(&self, bytes: u64) {
        let _ = self.metrics.record_bytes_sent(bytes);
        if let Some(stats) = &self.tracked {
            stats.sent::<Concurrent>(bytes, Instant::now());
        }
    }

    pub(crate) fn completed(&self, failed: bool) {
        if let Some(stats) = &self.tracked {
            stats.completed::<Concurrent>(failed, Instant::now());
        }
    }

    pub(crate) fn upstream(&self, upstream: Arc<str>) {
        if let Some(stats) = &self.tracked {
            stats.set_upstream(upstream);
        }
    }
}

pub(super) fn describe_quic_tls(stats: &ConnStats, connection: &quinn::Connection) {
    let Some(handshake) = connection
        .handshake_data()
        .and_then(|data| data.downcast::<HandshakeData>().ok())
    else {
        return;
    };
    stats.set_tls(TlsInfo {
        version: "TLS1.3",
        sni: handshake.server_name,
        alpn: handshake
            .protocol
            .and_then(|protocol| String::from_utf8(protocol).ok()),
    });
}
