//! Process-wide registry of HTTP requests currently inside the router.
//!
//! The access log is written on completion, so a request that never completes leaves no trace there.
//! This registry makes it visible while it is alive: the `InflightGuard` held by the request context
//! removes the entry on `Drop`, so failed and cancelled requests leave it too.

use std::{
    collections::BTreeMap,
    collections::HashMap,
    net::IpAddr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use http::Method;
use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    http::{
        action::AccessLog,
        evacuation::{Evacuation, EvacuationReport, Fate},
    },
    observability::access_record::{Correlation, Count, Safe, SafeId, StuckAccess},
    sharded::{self, SHARDS, Sharded},
};

pub(crate) const MAX_PATH_BYTES: usize = 128;
pub(crate) const OVERDUE_GRACE: Duration = Duration::from_secs(5);
pub(crate) const DEFAULT_STUCK_AFTER: Duration = Duration::from_mins(1);
const WATCHDOG_TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Routed,
    Queued,
    Connecting,
    AwaitingHeaders,
    Streaming,
    RetryWait,
}

impl Phase {
    const ALL: [Self; 6] = [
        Self::Routed,
        Self::Queued,
        Self::Connecting,
        Self::AwaitingHeaders,
        Self::Streaming,
        Self::RetryWait,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Routed => "routed",
            Self::Queued => "queued",
            Self::Connecting => "connecting",
            Self::AwaitingHeaders => "awaiting_headers",
            Self::Streaming => "streaming",
            Self::RetryWait => "retry_wait",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.as_str() == value)
    }

    fn from_u8(value: u8) -> Self {
        Self::ALL
            .get(usize::from(value))
            .copied()
            .unwrap_or(Self::Routed)
    }
}

#[derive(Default)]
struct Place {
    route: Option<String>,
    pool: Option<String>,
    server: Option<String>,
}

pub(crate) struct InflightEntry {
    key: u64,
    request_id: String,
    listener: String,
    method: Method,
    host: Option<String>,
    path: String,
    client_ip: Option<IpAddr>,
    started: Instant,
    phase: AtomicU8,
    phase_since_ms: AtomicU64,
    progress_ms: AtomicU64,
    attempts: AtomicU32,
    deadline_ms: AtomicU64,
    queue_position: Arc<AtomicU64>,
    stuck_reported: AtomicBool,
    place: Mutex<Place>,
    evacuation: Arc<Evacuation>,
    access_log: Option<Arc<AccessLog>>,
}

/// What the request looks like on arrival; everything else is learned while it is routed.
pub(crate) struct InflightArrival {
    pub(crate) request_id: String,
    pub(crate) listener: String,
    pub(crate) method: Method,
    pub(crate) host: Option<String>,
    pub(crate) path: String,
    pub(crate) client_ip: Option<IpAddr>,
    /// When the request arrived, so the registry reads the clock no more than the proxy already did.
    pub(crate) started: Instant,
    pub(crate) deadline: Duration,
    pub(crate) access_log: Option<Arc<AccessLog>>,
}

/// Path without the query, cut at `MAX_PATH_BYTES` on a character boundary.
pub(crate) fn bounded_path(path: &str) -> String {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let end = (0..=MAX_PATH_BYTES.min(path.len()))
        .rev()
        .find(|index| path.is_char_boundary(*index))
        .unwrap_or(0);
    path[..end].to_owned()
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl InflightEntry {
    fn age_ms(&self) -> u64 {
        elapsed_ms(self.started)
    }

    /// A request holding a lease that waits for capacity is queued, whatever step the proxy is in.
    fn phase(&self) -> Phase {
        match Phase::from_u8(self.phase.load(Ordering::Relaxed)) {
            Phase::Connecting if self.queue_position.load(Ordering::Relaxed) > 0 => Phase::Queued,
            phase => phase,
        }
    }

    fn place(&self) -> std::sync::MutexGuard<'_, Place> {
        self.place
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Streaming requests are stuck when the body stops moving; all others by total age.
    fn silent_ms(&self) -> u64 {
        let age = self.age_ms();
        if self.phase() == Phase::Streaming {
            age.saturating_sub(self.progress_ms.load(Ordering::Relaxed))
        } else {
            age
        }
    }

    fn overdue(&self) -> bool {
        let deadline = self.deadline_ms.load(Ordering::Relaxed);
        self.age_ms() > deadline.saturating_add(duration_ms(OVERDUE_GRACE))
    }

    /// The arrival proof a completion-only log cannot give: one line when the request goes quiet.
    fn write_stuck_line(&self, view: &InflightView) {
        let Some(log) = &self.access_log else {
            return;
        };
        let line = StuckAccess {
            age_ms: Count(view.age_ms),
            correlation_id: Correlation(Some(&view.request_id)),
            event: SafeId("stuck"),
            phase: view.phase.as_str().into(),
            pool: view.pool.as_deref().map(Safe::from),
            request_id: Some(Safe::from(view.request_id.as_str())),
            route: view.route.as_deref().map(Safe::from),
            server: view.server.as_deref().map(Safe::from),
            service: log.service().into(),
            timestamp_unix_ms: unix_ms(),
        };
        if let Err(error) = log.write_record(&line) {
            log::warn!("stuck request access log write failed: {error}");
        }
    }

    /// Acts on the attempt running on this pool and server. The place stays locked across the
    /// compare-and-swap, so an attempt that just moved on to another server is never taken by mistake.
    fn evacuate_on(
        &self,
        pool: &str,
        server: &str,
        force: bool,
        has_destination: bool,
    ) -> Option<Fate> {
        let place = self.place();
        let here = place.pool.as_deref() == Some(pool) && place.server.as_deref() == Some(server);
        here.then(|| self.evacuation.evacuate(force, has_destination))
            .flatten()
    }

    pub(crate) fn view(&self) -> InflightView {
        let place = self.place();
        let phase = self.phase();
        let age_ms = self.age_ms();
        InflightView {
            request_id: self.request_id.clone(),
            listener: self.listener.clone(),
            route: place.route.clone(),
            pool: place.pool.clone(),
            server: place.server.clone(),
            method: self.method.to_string(),
            host: self.host.clone(),
            path: self.path.clone(),
            client_ip: self.client_ip.map(|ip| ip.to_string()),
            phase,
            started_at_unix_ms: unix_ms().saturating_sub(age_ms),
            age_ms,
            waited_ms: (phase == Phase::Queued)
                .then(|| age_ms.saturating_sub(self.phase_since_ms.load(Ordering::Relaxed))),
            queue_position: Some(self.queue_position.load(Ordering::Relaxed)).filter(|p| *p > 0),
            attempts: self.attempts.load(Ordering::Relaxed),
            overdue: self.overdue(),
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, duration_ms)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InflightView {
    pub(crate) request_id: String,
    pub(crate) listener: String,
    pub(crate) route: Option<String>,
    pub(crate) pool: Option<String>,
    pub(crate) server: Option<String>,
    pub(crate) method: String,
    pub(crate) host: Option<String>,
    pub(crate) path: String,
    pub(crate) client_ip: Option<String>,
    pub(crate) phase: Phase,
    pub(crate) started_at_unix_ms: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    #[schemars(with = "String")]
    pub(crate) age_ms: u64,
    #[serde(
        serialize_with = "serialize_optional_u64_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub(crate) waited_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) queue_position: Option<u64>,
    pub(crate) attempts: u32,
    pub(crate) overdue: bool,
}

#[allow(
    clippy::ref_option,
    reason = "signature required by serde(serialize_with)"
)]
fn serialize_optional_u64_string<S: serde::Serializer>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serializer.serialize_str(&value.to_string()),
        None => serializer.serialize_none(),
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InflightFilter {
    pub(crate) pool: Option<String>,
    pub(crate) route: Option<String>,
    pub(crate) phase: Option<Phase>,
    pub(crate) min_age_ms: u64,
}

impl InflightFilter {
    fn accepts(&self, view: &InflightView) -> bool {
        self.pool
            .as_ref()
            .is_none_or(|pool| view.pool.as_ref() == Some(pool))
            && self
                .route
                .as_ref()
                .is_none_or(|route| view.route.as_ref() == Some(route))
            && self.phase.is_none_or(|phase| view.phase == phase)
            && view.age_ms >= self.min_age_ms
    }
}

pub(crate) struct InflightListing {
    pub(crate) total: usize,
    pub(crate) requests: Vec<InflightView>,
}

/// Per-pool live numbers for metrics and the status endpoint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PoolInflight {
    pub(crate) requests: u64,
    pub(crate) oldest_age_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct InflightTotals {
    pub(crate) pools: BTreeMap<String, PoolInflight>,
    pub(crate) overdue: u64,
    pub(crate) stuck: BTreeMap<(String, &'static str), u64>,
}

/// The requests one worker thread registered, and how many it has numbered.
#[derive(Default)]
struct Shard {
    entries: Mutex<HashMap<u64, Arc<InflightEntry>>>,
    registered: AtomicU64,
}

pub(crate) struct InflightRegistry {
    /// A request registers in the shard of the thread that received it, so workers do not contend; the
    /// shard number is the low bits of its key and the guard removes it from the same shard.
    shards: Sharded<Shard>,
    stuck: Mutex<BTreeMap<(String, &'static str), u64>>,
}

pub(crate) struct InflightGuard {
    entry: Arc<InflightEntry>,
    registry: &'static InflightRegistry,
}

/// Request id and phase of every request inside the router, for tests that watch the proxy from outside.
#[doc(hidden)]
#[must_use]
pub fn live_requests() -> Vec<(String, String)> {
    registry()
        .list(&InflightFilter::default(), usize::MAX)
        .requests
        .into_iter()
        .map(|view| (view.request_id, view.phase.as_str().to_owned()))
        .collect()
}

pub(crate) fn registry() -> &'static InflightRegistry {
    static REGISTRY: OnceLock<InflightRegistry> = OnceLock::new();
    static WATCHDOG: OnceLock<()> = OnceLock::new();
    let registry = REGISTRY.get_or_init(InflightRegistry::new);
    WATCHDOG.get_or_init(|| {
        std::thread::Builder::new()
            .name("oxiroute-inflight".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(WATCHDOG_TICK);
                    registry.report_stuck(DEFAULT_STUCK_AFTER);
                }
            })
            .expect("spawn in-flight watchdog");
    });
    registry
}

impl InflightRegistry {
    fn new() -> Self {
        Self {
            shards: Sharded::new(Shard::default),
            stuck: Mutex::new(BTreeMap::new()),
        }
    }

    fn lock(&self, index: usize) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<InflightEntry>>> {
        self.shards
            .get(index)
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn register(&'static self, arrival: InflightArrival) -> InflightGuard {
        let index = sharded::local_index();
        let number = self
            .shards
            .get(index)
            .registered
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let key = number * SHARDS as u64 + index as u64;
        let entry = Arc::new(InflightEntry {
            key,
            request_id: arrival.request_id,
            listener: arrival.listener,
            method: arrival.method,
            host: arrival.host,
            path: arrival.path,
            client_ip: arrival.client_ip,
            started: arrival.started,
            phase: AtomicU8::new(Phase::Routed as u8),
            phase_since_ms: AtomicU64::new(0),
            progress_ms: AtomicU64::new(0),
            attempts: AtomicU32::new(0),
            deadline_ms: AtomicU64::new(duration_ms(arrival.deadline)),
            queue_position: Arc::new(AtomicU64::new(0)),
            stuck_reported: AtomicBool::new(false),
            place: Mutex::new(Place::default()),
            evacuation: Arc::new(Evacuation::new()),
            access_log: arrival.access_log,
        });
        self.lock(index).insert(key, Arc::clone(&entry));
        InflightGuard {
            entry,
            registry: self,
        }
    }

    fn entries(&self) -> Vec<Arc<InflightEntry>> {
        (0..SHARDS)
            .flat_map(|index| self.lock(index).values().cloned().collect::<Vec<_>>())
            .collect()
    }

    pub(crate) fn list(&self, filter: &InflightFilter, limit: usize) -> InflightListing {
        let mut views = self
            .entries()
            .iter()
            .map(|entry| entry.view())
            .filter(|view| filter.accepts(view))
            .collect::<Vec<_>>();
        views.sort_by_key(|view| std::cmp::Reverse(view.age_ms));
        let total = views.len();
        views.truncate(limit);
        InflightListing {
            total,
            requests: views,
        }
    }

    /// Cancels the attempts running on `server` of `pool`, in any generation, and reports what happened.
    pub(crate) fn evacuate_server(
        &self,
        pool: &str,
        server: &str,
        force: bool,
        has_destination: bool,
    ) -> EvacuationReport {
        let mut report = EvacuationReport::default();
        for entry in self.entries() {
            if let Some(fate) = entry.evacuate_on(pool, server, force, has_destination) {
                report.record(fate);
            }
        }
        report
    }

    #[cfg(test)]
    pub(crate) fn find(&self, request_id: &str) -> Option<InflightView> {
        self.entries()
            .iter()
            .find(|entry| entry.request_id == request_id)
            .map(|entry| entry.view())
    }

    pub(crate) fn totals(&self) -> InflightTotals {
        let mut totals = InflightTotals {
            stuck: self
                .stuck
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            ..InflightTotals::default()
        };
        for entry in self.entries() {
            totals.overdue += u64::from(entry.overdue());
            let pool = entry.place().pool.clone().unwrap_or_default();
            let slot = totals.pools.entry(pool).or_default();
            slot.requests += 1;
            slot.oldest_age_ms = slot.oldest_age_ms.max(entry.age_ms());
        }
        totals
    }

    /// Counts and logs each stuck request once; the arrival line is what a completion log cannot give.
    pub(crate) fn report_stuck(&self, stuck_after: Duration) -> Vec<InflightView> {
        let limit = duration_ms(stuck_after);
        let mut reported = Vec::new();
        for entry in self.entries() {
            if entry.silent_ms() <= limit || entry.stuck_reported.swap(true, Ordering::Relaxed) {
                continue;
            }
            let view = entry.view();
            let pool = view.pool.clone().unwrap_or_default();
            *self
                .stuck
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry((pool, view.phase.as_str()))
                .or_default() += 1;
            log::warn!(
                "request {} stuck in {} for {} ms (pool {:?}, server {:?})",
                view.request_id,
                view.phase.as_str(),
                view.age_ms,
                view.pool,
                view.server
            );
            entry.write_stuck_line(&view);
            reported.push(view);
        }
        reported
    }
}

impl InflightGuard {
    /// `now` is the caller's reading of the clock for this step, so a phase change reads no clock of its own.
    pub(crate) fn set_phase(&self, phase: Phase, now: Instant) {
        let entry = &self.entry;
        let age = duration_ms(now.saturating_duration_since(entry.started));
        entry.phase.store(phase as u8, Ordering::Relaxed);
        entry.phase_since_ms.store(age, Ordering::Relaxed);
        entry.progress_ms.store(age, Ordering::Relaxed);
    }

    pub(crate) fn progress(&self) {
        self.entry
            .progress_ms
            .store(self.entry.age_ms(), Ordering::Relaxed);
    }

    pub(crate) fn set_route(&self, route: &str) {
        self.entry.place().route = Some(route.to_owned());
    }

    pub(crate) fn set_pool(&self, pool: &str) {
        self.entry.place().pool = Some(pool.to_owned());
    }

    pub(crate) fn set_server(&self, server: &str) {
        self.entry.attempts.fetch_add(1, Ordering::Relaxed);
        self.entry.place().server = Some(server.to_owned());
    }

    pub(crate) fn set_deadline(&self, deadline: Duration) {
        self.entry
            .deadline_ms
            .store(duration_ms(deadline), Ordering::Relaxed);
    }

    /// Where a lease reports its place in the pool queue, 0 when it is not waiting.
    pub(crate) fn queue_cell(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.entry.queue_position)
    }

    pub(crate) fn stuck(&self) -> bool {
        self.entry.stuck_reported.load(Ordering::Relaxed)
    }

    pub(crate) fn evacuation(&self) -> &Arc<Evacuation> {
        &self.entry.evacuation
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let index = usize::try_from(self.entry.key % SHARDS as u64).expect("shard index");
        self.registry.lock(index).remove(&self.entry.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::evacuation::{Cancellation, LeftRunning};

    fn arrival(request_id: &str, deadline: Duration) -> InflightArrival {
        InflightArrival {
            request_id: request_id.into(),
            listener: "edge".into(),
            method: Method::POST,
            host: Some("app.test".into()),
            path: "/v1/render".into(),
            client_ip: Some("192.0.2.9".parse().expect("test address")),
            started: Instant::now(),
            deadline,
            access_log: None,
        }
    }

    fn registry_for_test() -> &'static InflightRegistry {
        Box::leak(Box::new(InflightRegistry::new()))
    }

    #[test]
    fn an_entry_lives_exactly_as_long_as_its_guard() {
        let registry = registry_for_test();
        let guard = registry.register(arrival("a", Duration::from_secs(30)));
        guard.set_pool("farm");
        assert_eq!(registry.list(&InflightFilter::default(), 10).total, 1);
        assert_eq!(
            registry.find("a").expect("entry").pool.as_deref(),
            Some("farm")
        );
        drop(guard);
        assert_eq!(registry.list(&InflightFilter::default(), 10).total, 0);
        assert!(registry.find("a").is_none());
    }

    #[test]
    fn many_mixed_requests_leave_no_entries_behind() {
        let registry = registry_for_test();
        let mut guards = Vec::new();
        for index in 0..10_000 {
            let guard = registry.register(arrival(&format!("r{index}"), Duration::from_secs(1)));
            if index % 3 == 0 {
                guards.push(guard);
            }
        }
        assert_eq!(
            registry.list(&InflightFilter::default(), 1).total,
            guards.len()
        );
        drop(guards);
        assert_eq!(registry.list(&InflightFilter::default(), 1).total, 0);
    }

    #[test]
    fn phases_pool_server_and_attempts_follow_the_request() {
        let registry = registry_for_test();
        let guard = registry.register(arrival("p", Duration::from_secs(30)));
        for phase in [Phase::Queued, Phase::Connecting, Phase::AwaitingHeaders] {
            guard.set_phase(phase, Instant::now());
            assert_eq!(registry.find("p").expect("entry").phase, phase);
        }
        guard.set_server("gpu-1");
        guard.set_server("gpu-2");
        let view = registry.find("p").expect("entry");
        assert_eq!((view.server.as_deref(), view.attempts), (Some("gpu-2"), 2));
    }

    #[test]
    fn filters_select_by_pool_phase_and_age_oldest_first() {
        let registry = registry_for_test();
        let first = registry.register(arrival("old", Duration::from_secs(30)));
        first.set_pool("farm");
        std::thread::sleep(Duration::from_millis(20));
        let second = registry.register(arrival("new", Duration::from_secs(30)));
        second.set_pool("farm");
        second.set_phase(Phase::Queued, Instant::now());
        let other = registry.register(arrival("other", Duration::from_secs(30)));
        other.set_pool("web");
        let filter = InflightFilter {
            pool: Some("farm".into()),
            ..InflightFilter::default()
        };
        let listing = registry.list(&filter, 10);
        let ids = listing
            .requests
            .iter()
            .map(|view| view.request_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["old", "new"]);
        let queued = InflightFilter {
            phase: Some(Phase::Queued),
            ..InflightFilter::default()
        };
        assert_eq!(registry.list(&queued, 10).requests[0].request_id, "new");
        let aged = InflightFilter {
            min_age_ms: 10,
            ..InflightFilter::default()
        };
        assert_eq!(registry.list(&aged, 10).total, 1);
        assert_eq!(registry.list(&filter, 1).total, 2);
        assert_eq!(registry.list(&filter, 1).requests.len(), 1);
    }

    #[test]
    fn a_request_past_deadline_plus_grace_is_overdue() {
        let registry = registry_for_test();
        let guard = registry.register(arrival("late", Duration::ZERO));
        assert_eq!(registry.totals().overdue, 0);
        std::thread::sleep(OVERDUE_GRACE + Duration::from_millis(50));
        assert_eq!(registry.totals().overdue, 1);
        guard.set_deadline(Duration::from_mins(1));
        assert_eq!(registry.totals().overdue, 0);
    }

    #[test]
    fn a_silent_request_is_reported_stuck_exactly_once() {
        let registry = registry_for_test();
        let guard = registry.register(arrival("slow", Duration::from_secs(30)));
        guard.set_pool("farm");
        guard.set_phase(Phase::AwaitingHeaders, Instant::now());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(registry.report_stuck(Duration::from_millis(10)).len(), 1);
        assert!(registry.report_stuck(Duration::from_millis(10)).is_empty());
        assert!(guard.stuck());
        let totals = registry.totals();
        assert_eq!(
            totals.stuck.get(&("farm".to_owned(), "awaiting_headers")),
            Some(&1)
        );
    }

    #[test]
    fn a_streaming_request_that_keeps_moving_is_not_stuck() {
        let registry = registry_for_test();
        let guard = registry.register(arrival("stream", Duration::from_secs(30)));
        guard.set_phase(Phase::Streaming, Instant::now());
        std::thread::sleep(Duration::from_millis(30));
        guard.progress();
        assert!(registry.report_stuck(Duration::from_millis(20)).is_empty());
    }

    fn attempt_on(
        registry: &'static InflightRegistry,
        id: &str,
        pool: &str,
        server: &str,
        unmovable: Option<LeftRunning>,
    ) -> InflightGuard {
        let guard = registry.register(arrival(id, Duration::from_secs(30)));
        guard.set_pool(pool);
        guard.set_server(server);
        guard.evacuation().begin_attempt(unmovable);
        guard
    }

    #[test]
    fn evacuation_reaches_the_attempts_on_one_pool_and_server_of_any_generation() {
        let registry = registry_for_test();
        let movable = [
            attempt_on(registry, "old-generation", "farm", "node-a", None),
            attempt_on(registry, "new-generation", "farm", "node-a", None),
        ];
        let pinned = attempt_on(
            registry,
            "post",
            "farm",
            "node-a",
            Some(LeftRunning::NotReplayable),
        );
        let elsewhere = [
            attempt_on(registry, "other-server", "farm", "node-b", None),
            attempt_on(registry, "other-pool", "web", "node-a", None),
        ];
        let queued = registry.register(arrival("queued", Duration::from_secs(30)));
        queued.set_pool("farm");
        let retrying = attempt_on(registry, "retrying", "farm", "node-a", None);
        retrying.evacuation().settle();

        let report = registry.evacuate_server("farm", "node-a", false, true);

        assert_eq!(report.redispatched, 2);
        assert_eq!(report.forced, 0);
        assert_eq!(
            report.left_running.get(&LeftRunning::NotReplayable),
            Some(&1)
        );
        assert_eq!(report.left_running_total(), 1);
        assert!(
            movable
                .iter()
                .all(|guard| guard.evacuation().settle() == Some(Cancellation::Move))
        );
        assert_eq!(pinned.evacuation().settle(), None);
        assert!(
            elsewhere
                .iter()
                .all(|guard| guard.evacuation().settle().is_none())
        );
        assert_eq!(
            registry
                .evacuate_server("farm", "node-a", false, true)
                .redispatched,
            0
        );
    }

    #[test]
    fn a_forced_evacuation_kills_what_cannot_move_and_still_moves_the_rest() {
        let registry = registry_for_test();
        let streaming = attempt_on(
            registry,
            "stream",
            "farm",
            "node-a",
            Some(LeftRunning::ResponseStarted),
        );
        let movable = attempt_on(registry, "movable", "farm", "node-a", None);

        let report = registry.evacuate_server("farm", "node-a", true, true);

        assert_eq!(
            (
                report.redispatched,
                report.forced,
                report.left_running_total()
            ),
            (1, 1, 0)
        );
        assert_eq!(streaming.evacuation().settle(), Some(Cancellation::Kill));
        assert_eq!(movable.evacuation().settle(), Some(Cancellation::Move));
    }

    #[test]
    fn without_a_destination_movable_requests_stay_unless_forced() {
        let registry = registry_for_test();
        let movable = attempt_on(registry, "stuck-here", "farm", "node-a", None);

        let report = registry.evacuate_server("farm", "node-a", false, false);

        assert_eq!(
            report.left_running.get(&LeftRunning::NoDestination),
            Some(&1)
        );
        assert_eq!(movable.evacuation().settle(), None);
        let movable = attempt_on(registry, "cut-here", "farm", "node-a", None);

        let report = registry.evacuate_server("farm", "node-a", true, false);

        assert_eq!((report.forced, report.left_running_total()), (1, 0));
        assert_eq!(movable.evacuation().settle(), Some(Cancellation::Kill));
    }

    #[test]
    fn paths_drop_the_query_and_are_cut_on_a_character_boundary() {
        assert_eq!(bounded_path("/a/b?token=secret"), "/a/b");
        let long = format!("/{}", "é".repeat(100));
        let bounded = bounded_path(&long);
        assert!(bounded.len() <= MAX_PATH_BYTES);
        assert!(long.starts_with(&bounded));
    }

    #[test]
    fn a_stuck_request_writes_one_arrival_line_to_the_access_log() {
        use oxiroute_config::AccessLogPolicy;

        let directory = tempfile::tempdir().expect("log directory");
        let path = directory.path().join("access.jsonl");
        let log = AccessLog::open("svc", Some(&AccessLogPolicy::File { path: path.clone() }))
            .expect("open access log")
            .expect("access log enabled");
        let registry = registry_for_test();
        let mut stuck = arrival("lost-1", Duration::from_secs(30));
        stuck.access_log = Some(Arc::new(log));
        let guard = registry.register(stuck);
        guard.set_pool("farm");
        guard.set_phase(Phase::AwaitingHeaders, Instant::now());
        std::thread::sleep(Duration::from_millis(30));
        registry.report_stuck(Duration::from_millis(10));
        registry.report_stuck(Duration::from_millis(10));
        let mut lines = Vec::new();
        for _ in 0..100 {
            lines = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            if !lines.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("stuck line");
        assert_eq!(line["event"], "stuck");
        assert_eq!(line["requestId"], "lost-1");
        assert_eq!(line["phase"], "awaiting_headers");
        assert_eq!(line["pool"], "farm");
        assert!(line["ageMs"].is_string());
    }
}
