//! Per-pool and per-server request accounting: responses by class, attempt outcomes, latency, bytes
//! and the time requests spend waiting for capacity.
//!
//! Counters live outside the generations so a reload does not reset them; the Prometheus renderer
//! only reads the servers of the active pools, so removed servers disappear from the output.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::{
    EvacuationReport, LatencyBucketSnapshot, LatencySnapshot, OPERATION_LATENCY_BUCKETS_MS,
};

/// Request latency bounds plus the waits a saturated pool can produce.
pub(crate) const QUEUE_WAIT_BUCKETS_MS: &[u64] = &[
    1, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000, 120_000, 300_000,
    600_000,
];

pub(crate) const RESPONSE_CLASSES: [&str; 6] = ["1xx", "2xx", "3xx", "4xx", "5xx", "none"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttemptOutcome {
    Ok,
    ConnectFailed,
    Timeout,
    ProtocolError,
    StatusRetry,
    Rescued,
    ClientClosed,
    Evacuated,
}

impl AttemptOutcome {
    pub(crate) const ALL: [Self; 8] = [
        Self::Ok,
        Self::ConnectFailed,
        Self::Timeout,
        Self::ProtocolError,
        Self::StatusRetry,
        Self::Rescued,
        Self::ClientClosed,
        Self::Evacuated,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::ConnectFailed => "connect_failed",
            Self::Timeout => "timeout",
            Self::ProtocolError => "protocol_error",
            Self::StatusRetry => "status_retry",
            Self::Rescued => "rescued",
            Self::ClientClosed => "client_closed",
            Self::Evacuated => "evacuated",
        }
    }
}

/// What an evacuation did to the requests of a server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EvacuationResult {
    Redispatched,
    LeftRunning,
    Forced,
    /// A moved request that ended with an error instead of a response.
    Failed,
}

impl EvacuationResult {
    pub(crate) const ALL: [Self; 4] = [
        Self::Redispatched,
        Self::LeftRunning,
        Self::Forced,
        Self::Failed,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Redispatched => "redispatched",
            Self::LeftRunning => "left_running",
            Self::Forced => "forced",
            Self::Failed => "failed",
        }
    }
}

pub(crate) struct Histogram {
    bounds: &'static [u64],
    buckets: Box<[AtomicU64]>,
    count: AtomicU64,
    sum_ms: AtomicU64,
}

impl Histogram {
    pub(crate) fn new(bounds: &'static [u64]) -> Self {
        Self {
            bounds,
            buckets: (0..=bounds.len()).map(|_| AtomicU64::new(0)).collect(),
            count: AtomicU64::new(0),
            sum_ms: AtomicU64::new(0),
        }
    }

    pub(crate) fn observe(&self, duration: Duration) {
        let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let first = self
            .bounds
            .iter()
            .position(|bound| millis <= *bound)
            .unwrap_or(self.bounds.len());
        for bucket in &self.buckets[first..] {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ms.fetch_add(millis, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> LatencySnapshot {
        LatencySnapshot {
            buckets: self
                .buckets
                .iter()
                .enumerate()
                .map(|(index, count)| LatencyBucketSnapshot {
                    upper_bound_ms: self.bounds.get(index).copied(),
                    count: count.load(Ordering::Relaxed),
                })
                .collect(),
            count: self.count.load(Ordering::Relaxed),
            sum_ms: self.sum_ms.load(Ordering::Relaxed),
        }
    }
}

pub(crate) fn response_class_index(status: Option<u16>) -> usize {
    match status {
        Some(100..=199) => 0,
        Some(200..=299) => 1,
        Some(300..=399) => 2,
        Some(400..=499) => 3,
        Some(500..=599) => 4,
        _ => 5,
    }
}

pub(crate) struct ServerStats {
    responses: [AtomicU64; RESPONSE_CLASSES.len()],
    attempts: [AtomicU64; AttemptOutcome::ALL.len()],
    evacuated: [AtomicU64; EvacuationResult::ALL.len()],
    request_duration: Histogram,
    header_duration: Histogram,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
}

pub(crate) struct ServerStatsSnapshot {
    pub(crate) responses: [u64; RESPONSE_CLASSES.len()],
    pub(crate) attempts: [u64; AttemptOutcome::ALL.len()],
    pub(crate) evacuated: [u64; EvacuationResult::ALL.len()],
    pub(crate) request_duration: LatencySnapshot,
    pub(crate) header_duration: LatencySnapshot,
    pub(crate) bytes_sent: u64,
    pub(crate) bytes_received: u64,
}

impl ServerStats {
    fn new() -> Self {
        Self {
            responses: std::array::from_fn(|_| AtomicU64::new(0)),
            attempts: std::array::from_fn(|_| AtomicU64::new(0)),
            evacuated: std::array::from_fn(|_| AtomicU64::new(0)),
            request_duration: Histogram::new(OPERATION_LATENCY_BUCKETS_MS),
            header_duration: Histogram::new(OPERATION_LATENCY_BUCKETS_MS),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_response(&self, status: Option<u16>) {
        self.responses[response_class_index(status)].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_attempt(&self, outcome: AttemptOutcome) {
        let index = AttemptOutcome::ALL
            .iter()
            .position(|candidate| *candidate == outcome)
            .expect("listed outcome");
        self.attempts[index].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_evacuation(&self, result: EvacuationResult, requests: u64) {
        let index = EvacuationResult::ALL
            .iter()
            .position(|candidate| *candidate == result)
            .expect("listed result");
        self.evacuated[index].fetch_add(requests, Ordering::Relaxed);
    }

    pub(crate) fn record_evacuation_report(&self, report: &EvacuationReport) {
        self.record_evacuation(EvacuationResult::Redispatched, report.redispatched);
        self.record_evacuation(EvacuationResult::LeftRunning, report.left_running_total());
        self.record_evacuation(EvacuationResult::Forced, report.forced);
    }

    pub(crate) fn record_header_duration(&self, duration: Duration) {
        self.header_duration.observe(duration);
    }

    pub(crate) fn record_request_duration(&self, duration: Duration) {
        self.request_duration.observe(duration);
    }

    pub(crate) fn record_bytes(&self, sent: u64, received: u64) {
        self.bytes_sent.fetch_add(sent, Ordering::Relaxed);
        self.bytes_received.fetch_add(received, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> ServerStatsSnapshot {
        ServerStatsSnapshot {
            responses: std::array::from_fn(|index| self.responses[index].load(Ordering::Relaxed)),
            attempts: std::array::from_fn(|index| self.attempts[index].load(Ordering::Relaxed)),
            evacuated: std::array::from_fn(|index| self.evacuated[index].load(Ordering::Relaxed)),
            request_duration: self.request_duration.snapshot(),
            header_duration: self.header_duration.snapshot(),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
        }
    }
}

pub(crate) struct PoolStats {
    queue_wait: Histogram,
}

impl PoolStats {
    fn new() -> Self {
        Self {
            queue_wait: Histogram::new(QUEUE_WAIT_BUCKETS_MS),
        }
    }

    pub(crate) fn record_queue_wait(&self, waited: Duration) {
        self.queue_wait.observe(waited);
    }

    pub(crate) fn queue_wait(&self) -> LatencySnapshot {
        self.queue_wait.snapshot()
    }
}

#[derive(Default)]
pub(crate) struct UpstreamStats {
    pools: Mutex<HashMap<String, Arc<PoolStats>>>,
    servers: Mutex<HashMap<(String, String), Arc<ServerStats>>>,
}

pub(crate) fn upstream_stats() -> &'static UpstreamStats {
    static STATS: OnceLock<UpstreamStats> = OnceLock::new();
    STATS.get_or_init(UpstreamStats::default)
}

impl UpstreamStats {
    pub(crate) fn pool(&self, pool: &str) -> Arc<PoolStats> {
        let mut pools = self
            .pools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stats) = pools.get(pool) {
            return Arc::clone(stats);
        }
        let stats = Arc::new(PoolStats::new());
        pools.insert(pool.to_owned(), Arc::clone(&stats));
        stats
    }

    pub(crate) fn server(&self, pool: &str, server: &str) -> Arc<ServerStats> {
        let mut servers = self
            .servers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (pool.to_owned(), server.to_owned());
        Arc::clone(
            servers
                .entry(key)
                .or_insert_with(|| Arc::new(ServerStats::new())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histograms_are_cumulative_and_keep_the_overflow_bucket() {
        let histogram = Histogram::new(QUEUE_WAIT_BUCKETS_MS);
        histogram.observe(Duration::from_millis(3));
        histogram.observe(Duration::from_secs(90));
        histogram.observe(Duration::from_mins(15));
        let snapshot = histogram.snapshot();
        assert_eq!((snapshot.count, snapshot.sum_ms), (3, 3 + 90_000 + 900_000));
        let at = |bound: u64| {
            snapshot
                .buckets
                .iter()
                .find(|bucket| bucket.upper_bound_ms == Some(bound))
                .expect("bucket")
                .count
        };
        assert_eq!(
            (at(1), at(5), at(60_000), at(120_000), at(600_000)),
            (0, 1, 1, 2, 2)
        );
        assert_eq!(snapshot.buckets.last().expect("+Inf").count, 3);
    }

    #[test]
    fn statuses_map_to_bounded_classes() {
        let classes = [
            Some(101),
            Some(204),
            Some(302),
            Some(404),
            Some(503),
            None,
            Some(999),
        ]
        .map(response_class_index);
        assert_eq!(classes, [0, 1, 2, 3, 4, 5, 5]);
    }

    #[test]
    fn evacuations_are_counted_per_server_and_result() {
        let stats = UpstreamStats::default();
        let server = stats.server("farm", "node-a");
        server.record_evacuation(EvacuationResult::Redispatched, 3);
        server.record_evacuation(EvacuationResult::LeftRunning, 2);
        server.record_evacuation(EvacuationResult::Failed, 1);
        server.record_attempt(AttemptOutcome::Evacuated);
        let snapshot = stats.server("farm", "node-a").snapshot();
        assert_eq!(snapshot.evacuated, [3, 2, 0, 1]);
        assert_eq!(snapshot.attempts[7], 1);
        assert_eq!(stats.server("farm", "node-b").snapshot().evacuated, [0; 4]);
        assert_eq!(AttemptOutcome::Evacuated.as_str(), "evacuated");
    }

    #[test]
    fn servers_are_counted_per_pool_and_name() {
        let stats = UpstreamStats::default();
        stats.server("farm", "gpu-1").record_response(Some(200));
        stats.server("farm", "gpu-1").record_response(Some(500));
        stats.server("web", "gpu-1").record_response(Some(200));
        stats
            .server("farm", "gpu-1")
            .record_attempt(AttemptOutcome::Timeout);
        stats.server("farm", "gpu-1").record_bytes(10, 20);
        let snapshot = stats.server("farm", "gpu-1").snapshot();
        assert_eq!(snapshot.responses, [0, 1, 0, 0, 1, 0]);
        assert_eq!(snapshot.attempts[2], 1);
        assert_eq!((snapshot.bytes_sent, snapshot.bytes_received), (10, 20));
        assert_eq!(stats.server("web", "gpu-1").snapshot().responses[1], 1);
    }
}
