//! Process-wide registry of HTTP requests currently inside the router.
//!
//! The access log is written on completion, so a request that never completes leaves no trace there.
//! This registry makes it visible while it is alive: the `InflightGuard` held by the request context
//! removes the entry on `Drop`, so failed and cancelled requests leave it too.

use std::{
    collections::BTreeMap,
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
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
    label::Label,
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

/// Where the request is; the names are the ones the plans already hold, so recording them copies nothing.
#[derive(Default)]
struct Place {
    route: Option<Arc<str>>,
    pool: Option<Arc<str>>,
    server: Option<Arc<str>>,
}

/// The request id, host and path of an arrival in one allocation. Each is bounded by `MAX_PATH_BYTES`.
struct Texts {
    all: Box<str>,
    id_end: u16,
    host_end: Option<u16>,
}

impl Texts {
    fn new(request_id: &str, host: Option<&str>, path: &str) -> Self {
        let (id, host, path) = (
            bounded_text(request_id),
            host.map(bounded_text),
            bounded_path(path),
        );
        let mut all = String::with_capacity(id.len() + host.map_or(0, str::len) + path.len());
        all.push_str(id);
        let id_end = all.len();
        if let Some(host) = host {
            all.push_str(host);
        }
        let host_end = host.map(|_| all.len());
        all.push_str(path);
        let bound =
            |end: usize| u16::try_from(end).expect("each text is bounded by MAX_PATH_BYTES");
        Self {
            all: all.into_boxed_str(),
            id_end: bound(id_end),
            host_end: host_end.map(bound),
        }
    }

    fn request_id(&self) -> &str {
        &self.all[..usize::from(self.id_end)]
    }

    fn host(&self) -> Option<&str> {
        self.host_end
            .map(|end| &self.all[usize::from(self.id_end)..usize::from(end)])
    }

    fn path(&self) -> &str {
        let start = self.host_end.unwrap_or(self.id_end);
        &self.all[usize::from(start)..]
    }
}

pub(crate) struct InflightEntry {
    key: u64,
    texts: Texts,
    listener: Arc<str>,
    method: Method,
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
pub(crate) struct InflightArrival<'a> {
    pub(crate) request_id: &'a str,
    pub(crate) listener: Arc<str>,
    pub(crate) method: Method,
    pub(crate) host: Option<&'a str>,
    /// With or without a query; the registry keeps the path.
    pub(crate) path: &'a str,
    pub(crate) client_ip: Option<IpAddr>,
    /// When the request arrived, so the registry reads the clock no more than the proxy already did.
    pub(crate) started: Instant,
    pub(crate) deadline: Duration,
    pub(crate) access_log: Option<Arc<AccessLog>>,
}

/// `text` cut at `MAX_PATH_BYTES` on a character boundary.
pub(crate) fn bounded_text(text: &str) -> &str {
    let end = (0..=MAX_PATH_BYTES.min(text.len()))
        .rev()
        .find(|index| text.is_char_boundary(*index))
        .unwrap_or(0);
    &text[..end]
}

/// Path without the query, cut at `MAX_PATH_BYTES` on a character boundary.
pub(crate) fn bounded_path(path: &str) -> &str {
    bounded_text(path.split_once('?').map_or(path, |(path, _)| path))
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

    /// Only the task that serves the request counts its attempts, one after the other.
    fn count_attempt(&self) {
        self.attempts
            .store(self.attempts.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
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
            request_id: self.texts.request_id().to_owned(),
            listener: self.listener.to_string(),
            route: place.route.as_deref().map(str::to_owned),
            pool: place.pool.as_deref().map(str::to_owned),
            server: place.server.as_deref().map(str::to_owned),
            method: self.method.to_string(),
            host: self.texts.host().map(str::to_owned),
            path: self.texts.path().to_owned(),
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

/// Spreads the sequential request numbers; they are not attacker chosen, so a keyed hash would only cost.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.write_u64(u64::from(*byte));
        }
    }

    fn write_u64(&mut self, number: u64) {
        self.0 = (self.0 ^ number).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

type Entries = HashMap<u64, Arc<InflightEntry>, BuildHasherDefault<KeyHasher>>;

/// The requests one worker thread registered, and how many it has numbered.
#[derive(Default)]
struct Shard {
    entries: Mutex<Entries>,
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

    fn lock(&self, index: usize) -> std::sync::MutexGuard<'_, Entries> {
        self.shards
            .get(index)
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn register(&'static self, arrival: InflightArrival<'_>) -> InflightGuard {
        self.insert(arrival, Place::default())
    }

    /// Like [`Self::register`] for a request whose route and pool are known on arrival, so placing them takes no
    /// lock.
    pub(crate) fn register_placed(
        &'static self,
        arrival: InflightArrival<'_>,
        route: Arc<str>,
        pool: Arc<str>,
    ) -> InflightGuard {
        let place = Place {
            route: Some(route),
            pool: Some(pool),
            server: None,
        };
        self.insert(arrival, place)
    }

    fn insert(&'static self, arrival: InflightArrival<'_>, place: Place) -> InflightGuard {
        let index = sharded::local_index();
        let number = sharded::next(&self.shards.get(index).registered);
        let key = number * SHARDS as u64 + index as u64;
        let entry = Arc::new(InflightEntry {
            key,
            texts: Texts::new(arrival.request_id, arrival.host, arrival.path),
            listener: arrival.listener,
            method: arrival.method,
            client_ip: arrival.client_ip,
            started: arrival.started,
            phase: AtomicU8::new(Phase::Routed as u8),
            phase_since_ms: AtomicU64::new(0),
            progress_ms: AtomicU64::new(0),
            attempts: AtomicU32::new(0),
            deadline_ms: AtomicU64::new(duration_ms(arrival.deadline)),
            queue_position: Arc::new(AtomicU64::new(0)),
            stuck_reported: AtomicBool::new(false),
            place: Mutex::new(place),
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
            .find(|entry| entry.texts.request_id() == request_id)
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
            let pool = entry.place().pool.as_deref().unwrap_or_default().to_owned();
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

    pub(crate) fn progress(&self, now: Instant) {
        let age = duration_ms(now.saturating_duration_since(self.entry.started));
        self.entry.progress_ms.store(age, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn set_pool(&self, pool: &str) {
        self.place_pool(pool.into());
    }

    #[cfg(test)]
    pub(crate) fn set_server(&self, server: &str) {
        self.entry.count_attempt();
        self.entry.place().server = Some(server.into());
    }

    pub(crate) fn place_route(&self, route: Arc<str>) {
        self.entry.place().route = Some(route);
    }

    pub(crate) fn place_pool(&self, pool: Arc<str>) {
        self.entry.place().pool = Some(pool);
    }

    /// An attempt starts on `server` of `pool`, in one step: the pool is left alone when the request is placed on it
    /// already.
    pub(crate) fn place_attempt(&self, pool: &Label, server: Arc<str>) {
        self.entry.count_attempt();
        let mut place = self.entry.place();
        if !place
            .pool
            .as_ref()
            .is_some_and(|placed| pool.is_shared(placed))
        {
            place.pool = Some(pool.shared());
        }
        place.server = Some(server);
    }

    pub(crate) fn set_deadline(&self, deadline: Duration) {
        self.entry
            .deadline_ms
            .store(duration_ms(deadline), Ordering::Relaxed);
    }

    /// Where a lease reports its place in the pool queue, 0 when it is not waiting.
    pub(crate) fn queue_cell(&self) -> &Arc<AtomicU64> {
        &self.entry.queue_position
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
mod tests;
