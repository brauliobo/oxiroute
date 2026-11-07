#[allow(clippy::wildcard_imports)]
use super::*;

use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// How one canary request ended.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanaryResult {
    /// The expected status, in time.
    Ok,
    /// The expected status, slower than `max_latency_ms`.
    Slow,
    /// A connection, protocol, timeout or status failure.
    Failed,
    /// Every selectable server was at capacity, so the canary did not take a slot from a client.
    Busy,
    /// No server was selectable; the pool's own availability says why.
    Unavailable,
}

impl CanaryResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Slow => "slow",
            Self::Failed => "failed",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
        }
    }

    #[must_use]
    pub const fn healthy(self) -> bool {
        matches!(self, Self::Ok | Self::Slow)
    }
}

/// One finished canary request.
#[derive(Clone, Debug)]
pub struct CanaryRun {
    pub result: CanaryResult,
    pub reason: Option<HealthFailure>,
    pub server: Option<String>,
    pub duration: Duration,
}

#[derive(Debug, Default)]
struct CanaryRecord {
    last: Option<(CanaryRun, u64)>,
    last_attempt_unix_ms: Option<u64>,
    last_success_unix_ms: Option<u64>,
    consecutive_failures: u64,
    runs: BTreeMap<&'static str, u64>,
}

/// The outcome of the canary runs of one pool: written by its runner, read by every status surface.
#[derive(Debug)]
pub struct CanaryState {
    interval: Duration,
    record: Mutex<CanaryRecord>,
}

/// What status pages and metrics show about a pool's canary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanarySnapshot {
    /// `None` until the first run that was not skipped for capacity.
    pub result: Option<CanaryResult>,
    pub reason: Option<HealthFailure>,
    pub server: Option<String>,
    pub duration_ms: u64,
    pub last_attempt_unix_ms: Option<u64>,
    pub last_success_unix_ms: Option<u64>,
    pub consecutive_failures: u64,
    pub interval_ms: u64,
    pub runs: BTreeMap<&'static str, u64>,
}

impl CanaryState {
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            record: Mutex::new(CanaryRecord::default()),
        }
    }

    fn record(&self) -> std::sync::MutexGuard<'_, CanaryRecord> {
        self.record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Stores a run and returns the result it replaces when that flips between answering correctly
    /// and not, so the caller can log the change once.
    pub fn complete(&self, run: CanaryRun) -> Option<(CanaryResult, CanaryResult)> {
        let now = now_unix_ms();
        let mut record = self.record();
        *record.runs.entry(run.result.as_str()).or_default() += 1;
        record.last_attempt_unix_ms = Some(now);
        match run.result {
            CanaryResult::Busy => return None,
            CanaryResult::Ok | CanaryResult::Slow => {
                record.last_success_unix_ms = Some(now);
                record.consecutive_failures = 0;
            }
            CanaryResult::Failed => record.consecutive_failures += 1,
            CanaryResult::Unavailable => {}
        }
        let result = run.result;
        let previous = record.last.replace((run, now)).map(|(run, _)| run.result);
        previous
            .filter(|previous| previous.healthy() != result.healthy())
            .map(|previous| (previous, result))
    }

    #[must_use]
    pub fn snapshot(&self) -> CanarySnapshot {
        let record = self.record();
        let last = record.last.as_ref().map(|(run, _)| run);
        CanarySnapshot {
            result: last.map(|run| run.result),
            reason: last.and_then(|run| run.reason),
            server: last.and_then(|run| run.server.clone()),
            duration_ms: last.map_or(0, |run| {
                u64::try_from(run.duration.as_millis()).unwrap_or(u64::MAX)
            }),
            last_attempt_unix_ms: record.last_attempt_unix_ms,
            last_success_unix_ms: record.last_success_unix_ms,
            consecutive_failures: record.consecutive_failures,
            interval_ms: u64::try_from(self.interval.as_millis()).unwrap_or(u64::MAX),
            runs: record.runs.clone(),
        }
    }
}

/// What the canary got when it asked the pool for a server.
pub enum CanarySelection {
    Leased(EndpointLease),
    Busy,
    Unavailable,
}

impl EndpointPool {
    /// Selects a server the way a client request would, without ever waiting for capacity.
    #[must_use]
    pub fn canary_selection(&self) -> CanarySelection {
        let attempt = self.select_server_excluding(&[]);
        match attempt.lease {
            Some(lease) => CanarySelection::Leased(lease),
            None if attempt.pool_available => CanarySelection::Busy,
            None => CanarySelection::Unavailable,
        }
    }

    /// Attaches the state the canary runner writes; later calls are ignored.
    pub fn attach_canary(&self, state: Arc<CanaryState>) {
        drop(self.health.canary.set(state));
    }
}
