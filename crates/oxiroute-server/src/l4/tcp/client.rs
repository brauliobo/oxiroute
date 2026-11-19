use std::{net::SocketAddr, sync::Arc, time::Instant};

use crate::{
    ListenerMetrics,
    clients::{ConnStats, Connection, Exclusive, Protocol},
};

pub struct TrackedClient(Option<Connection>);

impl TrackedClient {
    #[must_use]
    pub const fn untracked() -> Self {
        Self(None)
    }

    #[must_use]
    pub fn open(metrics: &ListenerMetrics, peer: Option<SocketAddr>) -> Self {
        Self(peer.and_then(|peer| metrics.track(peer, Protocol::Tcp)))
    }

    pub(super) fn stats(&self) -> Option<&Arc<ConnStats>> {
        self.0.as_ref().map(Connection::stats)
    }
}

#[derive(Clone, Copy)]
pub(super) struct RelayClient<'a>(Option<&'a Arc<ConnStats>>);

impl<'a> RelayClient<'a> {
    pub(super) const fn new(stats: Option<&'a Arc<ConnStats>>) -> Self {
        Self(stats)
    }

    pub(super) fn received(self, bytes: u64) {
        if let Some(stats) = self.0 {
            stats.received::<Exclusive>(bytes, Instant::now());
        }
    }

    pub(super) fn sent(self, bytes: u64) {
        if let Some(stats) = self.0 {
            stats.sent::<Exclusive>(bytes, Instant::now());
        }
    }

    pub(super) fn completed(self, failed: bool) {
        if let Some(stats) = self.0 {
            stats.completed::<Exclusive>(failed, Instant::now());
        }
    }

    pub(super) fn upstream(self, upstream: Arc<str>) {
        if let Some(stats) = self.0 {
            stats.set_upstream(upstream);
        }
    }
}
