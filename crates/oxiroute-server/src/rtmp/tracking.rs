use std::{net::SocketAddr, time::Instant};

use openssl::ssl::NameType;
use pingora::protocols::Stream;

use crate::{
    ListenerMetrics, RuntimeMetrics,
    clients::{ClientQuery, Connection, ConnectionQuery, Exclusive, Protocol, Sort, TlsInfo},
};

/// The registration of one RTMP session in the per-client table; it lives exactly as long as the session.
pub struct RtmpSessionTracking(Option<Connection>);

impl RtmpSessionTracking {
    /// Registers the session of `downstream` coming from `peer`; nothing is tracked without a peer address, on a
    /// control listener, or while client tracking is disabled.
    #[must_use]
    pub fn open(metrics: &ListenerMetrics, downstream: &Stream, peer: Option<SocketAddr>) -> Self {
        let connection = peer.and_then(|peer| metrics.track(peer, Protocol::Rtmp));
        if let Some(connection) = &connection
            && let Some(ssl) = downstream.get_ssl()
        {
            connection.stats().set_tls(TlsInfo {
                version: tls_version(ssl.version_str()),
                sni: ssl.servername(NameType::HOST_NAME).map(str::to_owned),
                alpn: None,
            });
        }
        Self(connection)
    }

    #[must_use]
    pub const fn untracked() -> Self {
        Self(None)
    }

    pub fn received(&self, bytes: u64) {
        if let Some(connection) = &self.0 {
            connection
                .stats()
                .received::<Exclusive>(bytes, Instant::now());
        }
    }

    pub fn sent(&self, bytes: u64) {
        if let Some(connection) = &self.0 {
            connection.stats().sent::<Exclusive>(bytes, Instant::now());
        }
    }

    pub fn finish(&self, failed: bool) {
        if let Some(connection) = &self.0 {
            connection
                .stats()
                .completed::<Exclusive>(failed, Instant::now());
        }
    }
}

/// What the per-client table holds about the tracked clients, for tests of the binary crate.
#[doc(hidden)]
#[derive(Debug, Default, Eq, PartialEq)]
pub struct ObservedClients {
    pub live_protocols: Vec<&'static str>,
    pub connections_total: u64,
    pub requests: u64,
    pub errors: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[doc(hidden)]
#[must_use]
pub fn observe_clients(metrics: &RuntimeMetrics) -> ObservedClients {
    let now = Instant::now();
    let registry = metrics.clients();
    let clients = registry.clients(
        &ClientQuery {
            limit: usize::MAX,
            sort: Sort::Bytes,
            ip_prefix: None,
        },
        now,
    );
    let connections = registry.connections(
        &ConnectionQuery {
            limit: usize::MAX,
            ip_prefix: None,
            listener: None,
        },
        now,
    );
    let mut observed = ObservedClients {
        live_protocols: connections
            .connections
            .iter()
            .map(|row| row.protocol.as_str())
            .collect(),
        ..ObservedClients::default()
    };
    for row in &clients.clients {
        observed.connections_total += row.connections_total;
        observed.requests += row.requests;
        observed.errors += row.errors;
        observed.bytes_in += row.bytes_in;
        observed.bytes_out += row.bytes_out;
    }
    observed
}

fn tls_version(version: &str) -> &'static str {
    if version.contains("1.3") {
        "TLS1.3"
    } else if version.contains("1.2") {
        "TLS1.2"
    } else {
        "TLS"
    }
}
