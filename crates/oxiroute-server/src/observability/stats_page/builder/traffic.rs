//! Raw sums that a server row and its pool row share, converted to wire types once.

use super::super::model::{
    AttemptStats, EvacuationStats, Histogram, LatencyStats, ResponseStats, SessionStats,
    TrafficStats,
};
use crate::{
    LatencySnapshot, OPERATION_LATENCY_BUCKETS_MS,
    observability::upstream_stats::ServerStatsSnapshot,
};

/// Cumulative counts at the finite bounds of a histogram, plus the totals.
#[derive(Clone, Default)]
pub(super) struct Hist {
    buckets: Vec<u64>,
    count: u64,
    sum_ms: u64,
}

impl Hist {
    pub(super) fn add(&mut self, other: &Self) {
        if self.buckets.is_empty() {
            self.buckets = vec![0; other.buckets.len()];
        }
        for (own, other) in self.buckets.iter_mut().zip(&other.buckets) {
            *own += other;
        }
        self.count += other.count;
        self.sum_ms += other.sum_ms;
    }

    /// `bounds` is the number of finite buckets, so an empty sum still has the documented shape.
    pub(super) fn to_wire(&self, bounds: usize) -> Histogram {
        let mut buckets = self.buckets.clone();
        buckets.resize(bounds, 0);
        Histogram {
            count: self.count.into(),
            sum_ms: self.sum_ms.into(),
            buckets: buckets.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&LatencySnapshot> for Hist {
    /// Drops the overflow bucket: it is always `count`.
    fn from(snapshot: &LatencySnapshot) -> Self {
        let finite = snapshot.buckets.len().saturating_sub(1);
        Self {
            buckets: snapshot.buckets[..finite]
                .iter()
                .map(|bucket| bucket.count)
                .collect(),
            count: snapshot.count,
            sum_ms: snapshot.sum_ms,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Totals {
    sessions_current: u64,
    responses: [u64; 6],
    attempts: [u64; 8],
    evacuated: [u64; 4],
    bytes_sent: u64,
    bytes_received: u64,
    request: Hist,
    header: Hist,
}

impl Totals {
    pub(super) fn of(stats: &ServerStatsSnapshot, sessions_current: u64) -> Self {
        Self {
            sessions_current,
            responses: stats.responses,
            attempts: stats.attempts,
            evacuated: stats.evacuated,
            bytes_sent: stats.bytes_sent,
            bytes_received: stats.bytes_received,
            request: (&stats.request_duration).into(),
            header: (&stats.header_duration).into(),
        }
    }

    pub(super) fn add(&mut self, other: &Self) {
        self.sessions_current += other.sessions_current;
        for (own, other) in self.responses.iter_mut().zip(other.responses) {
            *own += other;
        }
        for (own, other) in self.attempts.iter_mut().zip(other.attempts) {
            *own += other;
        }
        for (own, other) in self.evacuated.iter_mut().zip(other.evacuated) {
            *own += other;
        }
        self.bytes_sent += other.bytes_sent;
        self.bytes_received += other.bytes_received;
        self.request.add(&other.request);
        self.header.add(&other.header);
    }

    pub(super) fn traffic(&self) -> TrafficStats {
        let [c1xx, c2xx, c3xx, c4xx, c5xx, none] = self.responses.map(Into::into);
        let [
            ok,
            connect_failed,
            timeout,
            protocol_error,
            status_retry,
            rescued,
            client_closed,
            evacuated,
        ] = self.attempts.map(Into::into);
        let [redispatched, left_running, forced, failed] = self.evacuated.map(Into::into);
        TrafficStats {
            sessions: SessionStats {
                current: self.sessions_current,
                total: self.attempts.iter().sum::<u64>().into(),
            },
            bytes_sent: self.bytes_sent.into(),
            bytes_received: self.bytes_received.into(),
            responses: ResponseStats {
                c1xx,
                c2xx,
                c3xx,
                c4xx,
                c5xx,
                none,
            },
            attempts: AttemptStats {
                ok,
                connect_failed,
                timeout,
                protocol_error,
                status_retry,
                rescued,
                client_closed,
                evacuated,
            },
            evacuated: EvacuationStats {
                redispatched,
                left_running,
                forced,
                failed,
            },
        }
    }

    pub(super) fn latency(&self) -> LatencyStats {
        LatencyStats {
            request: self.request.to_wire(OPERATION_LATENCY_BUCKETS_MS.len()),
            header: self.header.to_wire(OPERATION_LATENCY_BUCKETS_MS.len()),
        }
    }
}
