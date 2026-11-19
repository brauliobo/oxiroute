use std::{sync::Arc, time::Instant};

use pingora::protocols::{SocketDigest, Stream, http::ServerSession};

use crate::{
    ListenerMetrics,
    clients::{self, Concurrent, ConnStats, Connection, Exclusive, Protocol},
};

#[derive(Clone, Default)]
pub struct ForwardClient(Option<Arc<Connection>>);

impl ForwardClient {
    #[must_use]
    pub fn track_http1(metrics: &ListenerMetrics, stream: &Stream) -> Self {
        Self::track(metrics, stream, Protocol::Http1)
    }

    #[must_use]
    pub fn track_http2(metrics: &ListenerMetrics, stream: &Stream) -> Self {
        Self::track(metrics, stream, Protocol::H2)
    }

    fn track(metrics: &ListenerMetrics, stream: &Stream, protocol: Protocol) -> Self {
        clients::track(metrics, stream);
        let client = Self::registered(stream.get_socket_digest().as_deref());
        client.set_protocol(protocol);
        client
    }

    pub(super) fn of_session(session: &ServerSession) -> Self {
        Self::registered(
            session
                .digest()
                .and_then(|digest| digest.socket_digest.as_deref()),
        )
    }

    fn registered(digest: Option<&SocketDigest>) -> Self {
        Self(
            digest
                .and_then(SocketDigest::tracking)
                .and_then(|slot| Arc::clone(slot).downcast::<Connection>().ok()),
        )
    }

    fn stats(&self) -> Option<&ConnStats> {
        self.0.as_deref().map(|connection| &**connection.stats())
    }

    pub fn received(&self, bytes: u64) {
        if let Some(stats) = self.stats() {
            stats.received::<Exclusive>(bytes, Instant::now());
        }
    }

    pub fn sent(&self, bytes: u64) {
        if let Some(stats) = self.stats() {
            stats.sent::<Exclusive>(bytes, Instant::now());
        }
    }

    pub(super) fn received_shared(&self, bytes: u64) {
        if let Some(stats) = self.stats() {
            stats.received::<Concurrent>(bytes, Instant::now());
        }
    }

    pub(super) fn sent_shared(&self, bytes: u64) {
        if let Some(stats) = self.stats() {
            stats.sent::<Concurrent>(bytes, Instant::now());
        }
    }

    pub(super) fn tcp_tunnel(&self) {
        self.set_protocol(Protocol::Tcp);
    }

    pub(super) fn udp_tunnel(&self) {
        self.set_protocol(Protocol::Udp);
    }

    fn set_protocol(&self, protocol: Protocol) {
        if let Some(stats) = self.stats() {
            stats.set_protocol(protocol);
        }
    }

    pub(super) fn exchange(&self) -> Exchange {
        Exchange {
            connection: self.0.clone(),
            failed: false,
        }
    }
}

pub(super) struct Exchange {
    connection: Option<Arc<Connection>>,
    failed: bool,
}

impl Exchange {
    pub(super) const fn fail(&mut self) {
        self.failed = true;
    }

    pub(super) fn answered(&mut self, status: http::StatusCode) {
        self.failed = status.is_server_error();
    }

    pub(super) const fn detach(&mut self) -> Self {
        Self {
            connection: self.connection.take(),
            failed: self.failed,
        }
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        if let Some(connection) = &self.connection {
            connection
                .stats()
                .completed::<Concurrent>(self.failed, Instant::now());
        }
    }
}
