use std::{sync::Arc, time::Instant};

use oxiroute_epoll_h1::{Outcome, Secured, Transfer};
use pingora::apps::ConnectionAdmission;

use crate::clients::{Connection, Counter, Exclusive, Protocol, TlsInfo};

pub(super) struct EngineAdmission {
    pub(super) held: ConnectionAdmission,
    pub(super) tracked: Option<Connection>,
}

impl EngineAdmission {
    pub(super) fn counter(&self) -> Option<Counter> {
        self.tracked
            .as_ref()
            .map(|tracked| Counter::Engine(Arc::clone(tracked.stats())))
    }

    pub(super) fn transferred(&self, transfer: Transfer, now: Instant) {
        if let Some(tracked) = &self.tracked {
            tracked
                .stats()
                .transferred::<Exclusive>(transfer.received, transfer.sent, now);
        }
    }

    pub(super) fn completed(&self, outcome: &Outcome, local_bytes: u64, now: Instant) {
        if let Some(tracked) = &self.tracked {
            let stats = tracked.stats();
            if local_bytes > 0 {
                stats.transferred::<Exclusive>(0, local_bytes, now);
            }
            let failed =
                outcome.failure.is_some() || outcome.status.is_some_and(|status| status >= 500);
            stats.completed::<Exclusive>(failed, now);
        }
    }

    pub(super) fn secured(&self, secured: Secured) {
        let Some(tracked) = &self.tracked else {
            return;
        };
        let stats = tracked.stats();
        stats.set_handshaking(false);
        let alpn = secured.alpn.and_then(|alpn| String::from_utf8(alpn).ok());
        if alpn.as_deref() == Some("h2") {
            stats.set_protocol(Protocol::H2);
        }
        stats.set_tls(TlsInfo {
            version: secured.version,
            sni: secured.sni,
            alpn,
        });
    }
}
