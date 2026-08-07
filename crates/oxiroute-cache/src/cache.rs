use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use tokio::sync::watch;

use crate::{
    BaseKey, CacheKey, CacheTimeline, Clock, KeyError, MonoTime, RequestKeyInput,
    ResponseRejection, ResponseTiming, SystemClock, Validators,
    policy::{
        ParseError, RequestMode, RequestPolicy, ResponsePolicy, ResponsePolicyInput,
        merge_not_modified_headers,
    },
};

const ENTRY_ACCOUNTING_OVERHEAD: usize = 128;
const HEADER_ACCOUNTING_OVERHEAD: usize = 32;
const TAG_ACCOUNTING_OVERHEAD: usize = 16;

/// Hard limits for all cache-owned allocations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheConfig {
    pub max_entries: usize,
    pub max_total_bytes: usize,
    pub max_object_bytes: usize,
    pub max_header_bytes: usize,
    pub max_header_fields: usize,
    pub max_body_bytes: usize,
    pub max_key_bytes: usize,
    pub max_vary_fields: usize,
    pub max_tags_per_entry: usize,
    pub max_tag_bytes: usize,
    pub max_in_flight: usize,
    pub max_followers_per_fill: usize,
    pub max_heuristic_freshness: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_total_bytes: 256 * 1024 * 1024,
            max_object_bytes: 8 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            max_header_fields: 256,
            max_body_bytes: 8 * 1024 * 1024,
            max_key_bytes: 16 * 1024,
            max_vary_fields: 32,
            max_tags_per_entry: 16,
            max_tag_bytes: 256,
            max_in_flight: 1_024,
            max_followers_per_fill: 1_024,
            max_heuristic_freshness: Duration::from_hours(24),
        }
    }
}

/// In-memory bounded shared cache.
#[derive(Clone)]
pub struct Cache {
    shared: Arc<Shared>,
}

struct Shared {
    identity: Arc<CacheIdentity>,
    config: CacheConfig,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

#[derive(Debug)]
struct CacheIdentity;

struct State {
    entries: HashMap<CacheKey, StoredEntry>,
    flights: HashMap<BaseKey, Flight>,
    bytes_used: usize,
    sequence: u64,
    stats: CacheStats,
}

struct StoredEntry {
    entry: PreparedEntry,
    last_access: u64,
}

struct Flight {
    generation: Arc<FlightGeneration>,
    signal: watch::Sender<FillOutcome>,
    followers: usize,
}

struct FlightGeneration;

/// Immutable insertion candidate produced only after policy and bound checks.
#[derive(Clone, Debug)]
pub struct PreparedEntry {
    identity: Arc<CacheIdentity>,
    pub(crate) key: CacheKey,
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) tags: Vec<Bytes>,
    pub(crate) policy: ResponsePolicy,
    pub(crate) validators: Validators,
    pub(crate) response_received: MonoTime,
    pub(crate) charge: usize,
}

pub(crate) struct RecoveredEntry {
    pub(crate) key: CacheKey,
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) tags: Vec<Bytes>,
    pub(crate) policy: ResponsePolicy,
    pub(crate) validators: Validators,
    pub(crate) response_received: MonoTime,
    pub(crate) charge: usize,
}

/// Complete bounded response input used for canonical timeline preparation.
pub struct CacheResponse<'a> {
    pub status: StatusCode,
    pub headers: &'a HeaderMap,
    pub body: Bytes,
    pub timing: ResponseTiming,
    pub tags: &'a [&'a [u8]],
}

impl PreparedEntry {
    #[must_use]
    pub const fn key(&self) -> &CacheKey {
        &self.key
    }

    #[must_use]
    pub const fn charge(&self) -> usize {
        self.charge
    }
}

/// A cache response snapshot. HEAD callers use the same object but suppress `body` on the wire.
#[derive(Clone, Debug)]
pub struct CachedResponse {
    pub key: CacheKey,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub age: Duration,
}

/// Cache lookup result and wire-facing cache status.
#[derive(Clone, Debug)]
pub enum Lookup {
    Bypass {
        status: LookupStatus,
    },
    Miss {
        status: LookupStatus,
        only_if_cached: bool,
        base: BaseKey,
    },
    Hit {
        status: LookupStatus,
        response: CachedResponse,
    },
    Revalidate {
        status: LookupStatus,
        response: CachedResponse,
        validators: Validators,
        stale_if_error: bool,
    },
}

/// Stable status vocabulary suitable for `Cache-Status` or access-log mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupStatus {
    Bypass,
    Miss,
    Hit,
    StaleWhileRevalidate,
    Stale,
    Revalidate,
    StaleIfError,
}

/// Monotonic counters and current bounded resource use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub bypasses: u64,
    pub revalidations: u64,
    pub stale_while_revalidate: u64,
    pub stale_hits: u64,
    pub stale_if_error: u64,
    pub stores: u64,
    pub rejected: u64,
    pub evictions: u64,
    pub purged: u64,
    pub fill_leaders: u64,
    pub fill_followers: u64,
    pub fill_cancelled: u64,
    pub entries: usize,
    pub bytes_used: usize,
    pub in_flight: usize,
}

/// Result of joining a collapsed-forwarding group.
pub enum FillJoin {
    Leader(FillGuard),
    Follower(FillWaiter),
    AtCapacity,
}

/// Generation-bound leader permit. Dropping it cancels the fill and wakes followers.
pub struct FillGuard {
    shared: Weak<Shared>,
    identity: Arc<CacheIdentity>,
    base: BaseKey,
    generation: Arc<FlightGeneration>,
    active: bool,
}

pub(crate) struct ClaimedEntry(PreparedEntry);

/// Follower notification handle. It contains no cache lock and is safe to await across I/O.
pub struct FillWaiter {
    receiver: watch::Receiver<FillOutcome>,
}

/// Terminal result broadcast to collapsed followers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FillOutcome {
    Filling,
    Stored,
    NotStored,
    Cancelled,
    Purged,
}

/// Insertion result from a generation-valid leader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOutcome {
    Stored { evicted: usize },
    GenerationLost,
}

/// Number of objects removed by a bounded purge operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PurgeResult {
    pub entries: usize,
    pub bytes: usize,
    pub fills_cancelled: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache configuration limits must be nonzero and internally consistent")]
    InvalidConfig,
    #[error("invalid request cache metadata")]
    InvalidRequest(#[source] ParseError),
    #[error(transparent)]
    InvalidKey(#[from] KeyError),
    #[error("response cannot be stored in the shared cache")]
    ResponseRejected(#[from] ResponseRejection),
    #[error("only GET responses can create cached representations")]
    HeadOrUnsupportedMethod,
    #[error("response headers exceed configured bounds")]
    HeadersTooLarge,
    #[error("response body exceeds configured bounds")]
    BodyTooLarge,
    #[error("cache object exceeds configured bounds")]
    ObjectTooLarge,
    #[error("surrogate tag is malformed or exceeds configured bounds")]
    InvalidTag,
    #[error("too many surrogate tags")]
    TooManyTags,
    #[error("304 response changes the representation Vary key")]
    VaryChanged,
    #[error("fill response does not belong to the fill's base key")]
    FillKeyMismatch,
    #[error("prepared entry belongs to a different cache storage identity")]
    PreparedEntryOwnerMismatch,
    #[error("fill key is not a bounded GET representation key")]
    InvalidFillKey,
    #[error("304 metadata cannot be merged")]
    InvalidNotModified,
}

impl Cache {
    /// Creates a cache using the process monotonic clock.
    ///
    /// # Errors
    ///
    /// Returns an error if any allocation bound is zero or object limits exceed total quota.
    pub fn new(config: CacheConfig) -> Result<Self, CacheError> {
        Self::with_clock(config, Arc::new(SystemClock::new()))
    }

    /// Creates a cache with an injected monotonic clock.
    ///
    /// # Errors
    ///
    /// Returns an error if any allocation bound is zero or object limits exceed total quota.
    pub fn with_clock(config: CacheConfig, clock: Arc<dyn Clock>) -> Result<Self, CacheError> {
        validate_config(&config)?;
        Ok(Self {
            shared: Arc::new(Shared {
                identity: Arc::new(CacheIdentity),
                config,
                clock,
                state: Mutex::new(State {
                    entries: HashMap::new(),
                    flights: HashMap::new(),
                    bytes_used: 0,
                    sequence: 0,
                    stats: CacheStats::default(),
                }),
            }),
        })
    }

    #[must_use]
    pub fn config(&self) -> &CacheConfig {
        &self.shared.config
    }

    /// Reads the cache's monotonic clock for [`ResponseTiming`](crate::ResponseTiming).
    #[must_use]
    pub fn now(&self) -> MonoTime {
        self.shared.clock.now()
    }

    /// Validates a complete upstream response and creates an insertion candidate.
    ///
    /// # Errors
    ///
    /// Returns an error when request/response policy is unsafe or any configured bound is exceeded.
    pub fn prepare(
        &self,
        request: RequestKeyInput<'_>,
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
        timing: ResponseTiming,
        tags: &[&[u8]],
    ) -> Result<PreparedEntry, CacheError> {
        let result = self.prepare_inner(
            request,
            CacheResponse {
                status,
                headers,
                body,
                timing,
                tags,
            },
            None,
        );
        if result.is_err() {
            let mut state = self.shared.lock();
            state.stats.rejected = state.stats.rejected.saturating_add(1);
        }
        result
    }

    /// Validates a response using one canonical TTL/grace/keep timeline.
    ///
    /// # Errors
    ///
    /// Returns an error when request/response policy is unsafe or any configured bound is exceeded.
    pub fn prepare_with_timeline(
        &self,
        request: RequestKeyInput<'_>,
        response: CacheResponse<'_>,
        timeline: &CacheTimeline,
    ) -> Result<PreparedEntry, CacheError> {
        let result = self.prepare_inner(request, response, Some(timeline));
        if result.is_err() {
            let mut state = self.shared.lock();
            state.stats.rejected = state.stats.rejected.saturating_add(1);
        }
        result
    }

    fn prepare_inner(
        &self,
        request: RequestKeyInput<'_>,
        response: CacheResponse<'_>,
        timeline: Option<&CacheTimeline>,
    ) -> Result<PreparedEntry, CacheError> {
        let CacheResponse {
            status,
            headers,
            body,
            timing,
            tags,
        } = response;
        if *request.method != Method::GET {
            return Err(CacheError::HeadOrUnsupportedMethod);
        }
        if timing.response_received < timing.request_started {
            return Err(ResponseRejection::InvalidTiming.into());
        }
        check_header_bounds(request.headers, &self.shared.config)?;
        check_header_bounds(headers, &self.shared.config)?;
        if body.len() > self.shared.config.max_body_bytes {
            return Err(CacheError::BodyTooLarge);
        }
        let (policy, vary, validators) = ResponsePolicy::evaluate(ResponsePolicyInput {
            request_headers: request.headers,
            status,
            headers,
            timing,
            max_heuristic_freshness: self.shared.config.max_heuristic_freshness,
            max_vary_fields: self.shared.config.max_vary_fields,
            max_vary_bytes: self.shared.config.max_header_bytes,
            timeline,
        })?;
        let base = BaseKey::new(request, self.shared.config.max_key_bytes)?;
        let key = CacheKey::new(
            base,
            request.headers,
            &vary,
            self.shared.config.max_key_bytes,
        )?;
        let tags = parse_tags(tags, &self.shared.config)?;
        let charge =
            object_charge(&key, headers, body.len(), &tags).ok_or(CacheError::ObjectTooLarge)?;
        if charge > self.shared.config.max_object_bytes
            || charge > self.shared.config.max_total_bytes
        {
            return Err(CacheError::ObjectTooLarge);
        }
        Ok(PreparedEntry {
            identity: Arc::clone(&self.shared.identity),
            key,
            status,
            headers: headers.clone(),
            body,
            tags,
            policy,
            validators,
            response_received: timing.response_received,
            charge,
        })
    }

    /// Builds a replacement candidate by merging a 304 response into a stored representation.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing key, prohibited 304 fields, changed Vary key, or invalid
    /// resulting cache metadata.
    pub fn prepare_not_modified(
        &self,
        request: RequestKeyInput<'_>,
        key: &CacheKey,
        not_modified: &HeaderMap,
        timing: ResponseTiming,
    ) -> Result<PreparedEntry, CacheError> {
        let existing = {
            let state = self.shared.lock();
            state.entries.get(key).map(|stored| stored.entry.clone())
        }
        .ok_or(CacheError::InvalidNotModified)?;
        let merged = merge_not_modified_headers(&existing.headers, not_modified)
            .map_err(|_| CacheError::InvalidNotModified)?;
        let tag_refs = existing.tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
        let prepared = self.prepare_inner(
            request,
            CacheResponse {
                status: existing.status,
                headers: &merged,
                body: existing.body,
                timing,
                tags: &tag_refs,
            },
            None,
        )?;
        if prepared.key != *key {
            return Err(CacheError::VaryChanged);
        }
        Ok(prepared)
    }

    /// Builds a canonical-timeline replacement by merging a 304 response into a stored object.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing key, prohibited 304 fields, changed Vary key, or invalid
    /// resulting cache metadata.
    pub fn prepare_not_modified_with_timeline(
        &self,
        request: RequestKeyInput<'_>,
        key: &CacheKey,
        not_modified: &HeaderMap,
        timing: ResponseTiming,
        timeline: &CacheTimeline,
    ) -> Result<PreparedEntry, CacheError> {
        let existing = {
            let state = self.shared.lock();
            state.entries.get(key).map(|stored| stored.entry.clone())
        }
        .ok_or(CacheError::InvalidNotModified)?;
        let merged = merge_not_modified_headers(&existing.headers, not_modified)
            .map_err(|_| CacheError::InvalidNotModified)?;
        let tag_refs = existing.tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
        let prepared = self.prepare_inner(
            request,
            CacheResponse {
                status: existing.status,
                headers: &merged,
                body: existing.body,
                timing,
                tags: &tag_refs,
            },
            Some(timeline),
        )?;
        if prepared.key != *key {
            return Err(CacheError::VaryChanged);
        }
        Ok(prepared)
    }

    /// Looks up one GET/HEAD request and applies request freshness constraints.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed request directives or key inputs.
    pub fn lookup(&self, request: RequestKeyInput<'_>) -> Result<Lookup, CacheError> {
        if check_header_bounds(request.headers, &self.shared.config).is_err() {
            let mut state = self.shared.lock();
            state.stats.bypasses = state.stats.bypasses.saturating_add(1);
            return Ok(Lookup::Bypass {
                status: LookupStatus::Bypass,
            });
        }
        let policy = RequestPolicy::evaluate(request.method, request.headers)
            .map_err(CacheError::InvalidRequest)?;
        if policy.mode == RequestMode::Bypass {
            let mut state = self.shared.lock();
            state.stats.bypasses = state.stats.bypasses.saturating_add(1);
            return Ok(Lookup::Bypass {
                status: LookupStatus::Bypass,
            });
        }
        let base = BaseKey::new(request, self.shared.config.max_key_bytes)?;
        let authorized = request.headers.contains_key(http::header::AUTHORIZATION);
        let now = self.shared.clock.now();
        let mut state = self.shared.lock();
        let found = matching_key(
            &state,
            &base,
            request.headers,
            self.shared.config.max_key_bytes,
            authorized,
        );
        let Some(key) = found else {
            return Ok(cache_miss(&mut state, &policy, base));
        };
        if remove_expired_entry(&mut state, &key, now) {
            return Ok(cache_miss(&mut state, &policy, base));
        }
        let access = state.next_sequence();
        let stored = state
            .entries
            .get_mut(&key)
            .ok_or(CacheError::InvalidNotModified)?;
        stored.last_access = access;
        let age = stored
            .entry
            .policy
            .corrected_initial_age
            .saturating_add(now.saturating_duration_since(stored.entry.response_received));
        let response = response_snapshot(&stored.entry, age);
        let request_fresh = policy.max_age.is_none_or(|maximum| age <= maximum);
        let fresh = age.saturating_add(policy.min_fresh) <= stored.entry.policy.freshness_lifetime;
        let forced = policy.mode == RequestMode::Revalidate
            || !request_fresh
            || !policy.min_fresh.is_zero() && !fresh
            || stored.entry.policy.always_revalidate
            || stored.entry.policy.must_revalidate_stale && !fresh;
        if fresh && request_fresh && !forced {
            state.stats.hits = state.stats.hits.saturating_add(1);
            return Ok(Lookup::Hit {
                status: LookupStatus::Hit,
                response,
            });
        }
        let staleness = age.saturating_sub(stored.entry.policy.freshness_lifetime);
        let request_allows_stale = stored.entry.policy.retention.request_stale_allowed()
            && !forced
            && !stored.entry.policy.must_revalidate_stale
            && policy
                .max_stale
                .is_some_and(|limit| limit.is_none_or(|limit| staleness <= limit));
        let swr = !forced
            && staleness <= stored.entry.policy.stale_while_revalidate
            && !stored.entry.policy.stale_while_revalidate.is_zero();
        if request_allows_stale || swr {
            let status = if request_allows_stale {
                state.stats.stale_hits = state.stats.stale_hits.saturating_add(1);
                LookupStatus::Stale
            } else {
                state.stats.stale_while_revalidate =
                    state.stats.stale_while_revalidate.saturating_add(1);
                LookupStatus::StaleWhileRevalidate
            };
            return Ok(Lookup::Hit { status, response });
        }
        let stale_if_error = policy.mode != RequestMode::Revalidate
            && !stored.entry.policy.must_revalidate_stale
            && staleness <= stored.entry.policy.stale_if_error
            && !stored.entry.policy.stale_if_error.is_zero();
        let validators = stored.entry.validators.clone();
        state.stats.revalidations = state.stats.revalidations.saturating_add(1);
        Ok(Lookup::Revalidate {
            status: LookupStatus::Revalidate,
            response,
            validators,
            stale_if_error,
        })
    }

    /// Records and returns an eligible stale-if-error representation by exact key.
    #[must_use]
    pub fn stale_if_error(&self, key: &CacheKey) -> Option<CachedResponse> {
        let now = self.shared.clock.now();
        let mut state = self.shared.lock();
        let response = {
            let stored = state.entries.get(key)?;
            let age = stored
                .entry
                .policy
                .corrected_initial_age
                .saturating_add(now.saturating_duration_since(stored.entry.response_received));
            let staleness = age.saturating_sub(stored.entry.policy.freshness_lifetime);
            if stored.entry.policy.must_revalidate_stale
                || stored.entry.policy.stale_if_error.is_zero()
                || staleness > stored.entry.policy.stale_if_error
            {
                return None;
            }
            response_snapshot(&stored.entry, age)
        };
        let access = state.next_sequence();
        state.entries.get_mut(key)?.last_access = access;
        state.stats.stale_if_error = state.stats.stale_if_error.saturating_add(1);
        Some(response)
    }

    /// Starts or joins one bounded collapsed-forwarding group. No mutex guard escapes this call.
    ///
    /// # Errors
    ///
    /// Returns an error if the caller supplies a non-GET or oversized base key.
    pub fn begin_fill(&self, base: BaseKey) -> Result<FillJoin, CacheError> {
        if !base.is_get() || base.encoded_len() > self.shared.config.max_key_bytes {
            return Err(CacheError::InvalidFillKey);
        }
        let mut state = self.shared.lock();
        if let Some(flight) = state.flights.get_mut(&base) {
            if flight.followers >= self.shared.config.max_followers_per_fill {
                return Ok(FillJoin::AtCapacity);
            }
            let receiver = flight.signal.subscribe();
            flight.followers += 1;
            state.stats.fill_followers = state.stats.fill_followers.saturating_add(1);
            return Ok(FillJoin::Follower(FillWaiter { receiver }));
        }
        if state.flights.len() >= self.shared.config.max_in_flight {
            return Ok(FillJoin::AtCapacity);
        }
        let generation = Arc::new(FlightGeneration);
        let (signal, _receiver) = watch::channel(FillOutcome::Filling);
        state.flights.insert(
            base.clone(),
            Flight {
                generation: Arc::clone(&generation),
                signal,
                followers: 0,
            },
        );
        state.stats.fill_leaders = state.stats.fill_leaders.saturating_add(1);
        Ok(FillJoin::Leader(FillGuard {
            shared: Arc::downgrade(&self.shared),
            identity: Arc::clone(&self.shared.identity),
            base,
            generation,
            active: true,
        }))
    }

    /// Purges one exact representation and cancels any fill for its base request.
    #[must_use]
    pub fn purge_exact(&self, key: &CacheKey) -> PurgeResult {
        let mut state = self.shared.lock();
        let mut result = PurgeResult::default();
        if let Some(stored) = state.entries.remove(key) {
            state.bytes_used = state.bytes_used.saturating_sub(stored.entry.charge);
            result.entries = 1;
            result.bytes = stored.entry.charge;
        }
        result.fills_cancelled =
            usize::from(cancel_flight(&mut state, key.base(), FillOutcome::Purged));
        state.stats.purged = state.stats.purged.saturating_add(result.entries as u64);
        result
    }

    /// Purges every representation for one bounded request key and cancels its active fill.
    #[must_use]
    pub fn purge_base(&self, base: &BaseKey) -> PurgeResult {
        let mut state = self.shared.lock();
        let keys = state
            .entries
            .keys()
            .filter(|key| key.base() == base)
            .cloned()
            .collect::<Vec<_>>();
        let mut result = PurgeResult::default();
        for key in keys {
            if let Some(stored) = state.entries.remove(&key) {
                state.bytes_used = state.bytes_used.saturating_sub(stored.entry.charge);
                result.entries += 1;
                result.bytes = result.bytes.saturating_add(stored.entry.charge);
            }
        }
        result.fills_cancelled = usize::from(cancel_flight(&mut state, base, FillOutcome::Purged));
        state.stats.purged = state.stats.purged.saturating_add(result.entries as u64);
        result
    }

    /// Purges entries carrying an exact bounded surrogate tag and cancels fills for affected bases.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied tag is malformed or exceeds the configured tag bound.
    pub fn purge_tag(&self, tag: &[u8]) -> Result<PurgeResult, CacheError> {
        validate_tag(tag, self.shared.config.max_tag_bytes)?;
        let mut state = self.shared.lock();
        let keys = state
            .entries
            .iter()
            .filter(|(_, stored)| {
                stored
                    .entry
                    .tags
                    .iter()
                    .any(|entry_tag| entry_tag.as_ref() == tag)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut result = PurgeResult::default();
        for key in keys {
            if let Some(stored) = state.entries.remove(&key) {
                state.bytes_used = state.bytes_used.saturating_sub(stored.entry.charge);
                result.entries += 1;
                result.bytes = result.bytes.saturating_add(stored.entry.charge);
            }
            result.fills_cancelled +=
                usize::from(cancel_flight(&mut state, key.base(), FillOutcome::Purged));
        }
        state.stats.purged = state.stats.purged.saturating_add(result.entries as u64);
        Ok(result)
    }

    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let state = self.shared.lock();
        CacheStats {
            entries: state.entries.len(),
            bytes_used: state.bytes_used,
            in_flight: state.flights.len(),
            ..state.stats
        }
    }

    pub(crate) fn entry(&self, key: &CacheKey) -> Option<PreparedEntry> {
        self.shared
            .lock()
            .entries
            .get(key)
            .map(|stored| stored.entry.clone())
    }

    pub(crate) fn resident_keys(&self) -> Vec<CacheKey> {
        self.shared.lock().entries.keys().cloned().collect()
    }

    pub(crate) fn restore(&self, entry: RecoveredEntry) {
        let entry = PreparedEntry {
            identity: Arc::clone(&self.shared.identity),
            key: entry.key,
            status: entry.status,
            headers: entry.headers,
            body: entry.body,
            tags: entry.tags,
            policy: entry.policy,
            validators: entry.validators,
            response_received: entry.response_received,
            charge: entry.charge,
        };
        let mut state = self.shared.lock();
        insert_entry(&self.shared.config, &mut state, entry);
    }

    pub(crate) fn remove_without_cancelling_fill(&self, key: &CacheKey) -> bool {
        let mut state = self.shared.lock();
        let Some(stored) = state.entries.remove(key) else {
            return false;
        };
        state.bytes_used = state.bytes_used.saturating_sub(stored.entry.charge);
        true
    }

    pub(crate) fn cancel_all_fills(&self) {
        let mut state = self.shared.lock();
        let count = state.flights.len();
        for (_, flight) in state.flights.drain() {
            flight.signal.send_replace(FillOutcome::Cancelled);
        }
        state.stats.fill_cancelled = state.stats.fill_cancelled.saturating_add(count as u64);
    }
}

impl FillGuard {
    pub(crate) fn is_current(&self) -> bool {
        self.shared.upgrade().is_some_and(|shared| {
            let state = shared.lock();
            generation_matches(&state, &self.base, &self.generation)
        })
    }

    /// Atomically publishes a prepared object only if this fill generation is still current.
    ///
    /// # Errors
    ///
    /// Returns an error if the object belongs to another cache or base key.
    pub fn store(self, entry: PreparedEntry) -> Result<StoreOutcome, CacheError> {
        let entry = self.claim(entry)?;
        Ok(self.store_claimed(entry))
    }

    pub(crate) fn claim(&self, entry: PreparedEntry) -> Result<ClaimedEntry, CacheError> {
        if !Arc::ptr_eq(&entry.identity, &self.identity) {
            return Err(CacheError::PreparedEntryOwnerMismatch);
        }
        if entry.key.base() != &self.base {
            return Err(CacheError::FillKeyMismatch);
        }
        Ok(ClaimedEntry(entry))
    }

    pub(crate) fn store_claimed(mut self, entry: ClaimedEntry) -> StoreOutcome {
        let Some(shared) = self.shared.upgrade() else {
            self.active = false;
            return StoreOutcome::GenerationLost;
        };
        let mut state = shared.lock();
        if !generation_matches(&state, &self.base, &self.generation) {
            self.active = false;
            return StoreOutcome::GenerationLost;
        }
        let flight = state.flights.remove(&self.base);
        let evicted = insert_entry(&shared.config, &mut state, entry.0);
        state.stats.stores = state.stats.stores.saturating_add(1);
        if let Some(flight) = flight {
            flight.signal.send_replace(FillOutcome::Stored);
        }
        self.active = false;
        StoreOutcome::Stored { evicted }
    }

    /// Completes a valid generation without storing a response and wakes followers.
    #[must_use]
    pub fn complete_without_store(mut self) -> bool {
        let completed = self.finish(FillOutcome::NotStored);
        self.active = false;
        completed
    }

    fn finish(&self, outcome: FillOutcome) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let mut state = shared.lock();
        if !generation_matches(&state, &self.base, &self.generation) {
            return false;
        }
        if let Some(flight) = state.flights.remove(&self.base) {
            flight.signal.send_replace(outcome);
            return true;
        }
        false
    }
}

impl ClaimedEntry {
    pub(crate) const fn prepared(&self) -> &PreparedEntry {
        &self.0
    }
}

impl Drop for FillGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(shared) = self.shared.upgrade() {
            let mut state = shared.lock();
            if generation_matches(&state, &self.base, &self.generation) {
                if let Some(flight) = state.flights.remove(&self.base) {
                    flight.signal.send_replace(FillOutcome::Cancelled);
                }
                state.stats.fill_cancelled = state.stats.fill_cancelled.saturating_add(1);
            }
        }
    }
}

impl FillWaiter {
    /// Waits for the leader's terminal result without holding a cache mutex.
    pub async fn wait(mut self) -> FillOutcome {
        loop {
            let outcome = *self.receiver.borrow_and_update();
            if outcome != FillOutcome::Filling {
                return outcome;
            }
            if self.receiver.changed().await.is_err() {
                return *self.receiver.borrow_and_update();
            }
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl State {
    fn next_sequence(&mut self) -> u64 {
        if self.sequence == u64::MAX {
            let mut order = self
                .entries
                .iter()
                .map(|(key, stored)| (stored.last_access, key.clone()))
                .collect::<Vec<_>>();
            order.sort_unstable();
            for (index, (_, key)) in order.into_iter().enumerate() {
                if let Some(stored) = self.entries.get_mut(&key) {
                    stored.last_access = index as u64;
                }
            }
            self.sequence = self.entries.len() as u64;
        }
        self.sequence += 1;
        self.sequence
    }
}

fn remove_expired_entry(state: &mut State, key: &CacheKey, now: MonoTime) -> bool {
    let expired =
        state.entries.get(key).is_some_and(|stored| {
            let age = stored
                .entry
                .policy
                .corrected_initial_age
                .saturating_add(now.saturating_duration_since(stored.entry.response_received));
            stored.entry.policy.retention.keep().is_some_and(|keep| {
                age > stored.entry.policy.freshness_lifetime.saturating_add(keep)
            })
        });
    if expired && let Some(stored) = state.entries.remove(key) {
        state.bytes_used = state.bytes_used.saturating_sub(stored.entry.charge);
    }
    expired
}

fn cache_miss(state: &mut State, policy: &RequestPolicy, base: BaseKey) -> Lookup {
    state.stats.misses = state.stats.misses.saturating_add(1);
    Lookup::Miss {
        status: LookupStatus::Miss,
        only_if_cached: policy.only_if_cached,
        base,
    }
}

fn matching_key(
    state: &State,
    base: &BaseKey,
    headers: &HeaderMap,
    max_key_bytes: usize,
    authorized: bool,
) -> Option<CacheKey> {
    state
        .entries
        .keys()
        .filter(|key| {
            key.matches_request(base, headers, max_key_bytes)
                && state.entries.get(*key).is_some_and(|stored| {
                    !authorized || stored.entry.policy.allows_authorized_reuse
                })
        })
        .max_by_key(|key| state.entries.get(*key).map_or(0, |entry| entry.last_access))
        .cloned()
}

fn insert_entry(config: &CacheConfig, state: &mut State, entry: PreparedEntry) -> usize {
    if let Some(old) = state.entries.remove(&entry.key) {
        state.bytes_used = state.bytes_used.saturating_sub(old.entry.charge);
    }
    let conflicting = state
        .entries
        .keys()
        .filter(|key| key.base() == entry.key.base() && !key.same_vary_schema(&entry.key))
        .cloned()
        .collect::<Vec<_>>();
    for key in conflicting {
        if let Some(old) = state.entries.remove(&key) {
            state.bytes_used = state.bytes_used.saturating_sub(old.entry.charge);
        }
    }
    let mut evicted = 0usize;
    while state.entries.len() >= config.max_entries
        || state.bytes_used.saturating_add(entry.charge) > config.max_total_bytes
    {
        let lru = state
            .entries
            .iter()
            .min_by(|(left_key, left), (right_key, right)| {
                (left.last_access, *left_key).cmp(&(right.last_access, *right_key))
            })
            .map(|(key, _)| key.clone());
        let Some(lru) = lru else {
            break;
        };
        if let Some(old) = state.entries.remove(&lru) {
            state.bytes_used = state.bytes_used.saturating_sub(old.entry.charge);
            evicted += 1;
        }
    }
    let last_access = state.next_sequence();
    state.bytes_used = state.bytes_used.saturating_add(entry.charge);
    state
        .entries
        .insert(entry.key.clone(), StoredEntry { entry, last_access });
    state.stats.evictions = state.stats.evictions.saturating_add(evicted as u64);
    evicted
}

fn response_snapshot(entry: &PreparedEntry, age: Duration) -> CachedResponse {
    let mut headers = entry.headers.clone();
    if let Ok(value) = HeaderValue::from_str(&age.as_secs().to_string()) {
        headers.insert(http::header::AGE, value);
    }
    CachedResponse {
        key: entry.key.clone(),
        status: entry.status,
        headers,
        body: entry.body.clone(),
        age,
    }
}

fn generation_matches(state: &State, base: &BaseKey, generation: &Arc<FlightGeneration>) -> bool {
    state
        .flights
        .get(base)
        .is_some_and(|flight| Arc::ptr_eq(&flight.generation, generation))
}

fn cancel_flight(state: &mut State, base: &BaseKey, outcome: FillOutcome) -> bool {
    if let Some(flight) = state.flights.remove(base) {
        flight.signal.send_replace(outcome);
        state.stats.fill_cancelled = state.stats.fill_cancelled.saturating_add(1);
        true
    } else {
        false
    }
}

fn validate_config(config: &CacheConfig) -> Result<(), CacheError> {
    let nonzero = config.max_entries > 0
        && config.max_total_bytes > 0
        && config.max_object_bytes > 0
        && config.max_header_bytes > 0
        && config.max_header_fields > 0
        && config.max_body_bytes > 0
        && config.max_key_bytes > 0
        && config.max_vary_fields > 0
        && config.max_tags_per_entry > 0
        && config.max_tag_bytes > 0
        && config.max_in_flight > 0
        && config.max_followers_per_fill > 0;
    if !nonzero
        || config.max_object_bytes > config.max_total_bytes
        || config.max_body_bytes > config.max_object_bytes
    {
        return Err(CacheError::InvalidConfig);
    }
    Ok(())
}

fn check_header_bounds(headers: &HeaderMap, config: &CacheConfig) -> Result<(), CacheError> {
    if headers.len() > config.max_header_fields {
        return Err(CacheError::HeadersTooLarge);
    }
    let bytes = headers.iter().try_fold(0usize, |size, (name, value)| {
        size.checked_add(name.as_str().len())?
            .checked_add(value.len())
    });
    if bytes.is_none_or(|bytes| bytes > config.max_header_bytes) {
        return Err(CacheError::HeadersTooLarge);
    }
    Ok(())
}

fn parse_tags(tags: &[&[u8]], config: &CacheConfig) -> Result<Vec<Bytes>, CacheError> {
    if tags.len() > config.max_tags_per_entry {
        return Err(CacheError::TooManyTags);
    }
    let mut parsed = Vec::with_capacity(tags.len());
    for tag in tags {
        validate_tag(tag, config.max_tag_bytes)?;
        let tag = Bytes::copy_from_slice(tag);
        if !parsed.contains(&tag) {
            parsed.push(tag);
        }
    }
    Ok(parsed)
}

pub(crate) fn validate_tag(tag: &[u8], max_bytes: usize) -> Result<(), CacheError> {
    if tag.is_empty()
        || tag.len() > max_bytes
        || !tag
            .iter()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b'"'))
    {
        return Err(CacheError::InvalidTag);
    }
    Ok(())
}

pub(crate) fn object_charge(
    key: &CacheKey,
    headers: &HeaderMap,
    body_bytes: usize,
    tags: &[Bytes],
) -> Option<usize> {
    let mut charge = ENTRY_ACCOUNTING_OVERHEAD.checked_add(key.encoded_len())?;
    for (name, value) in headers {
        charge = charge
            .checked_add(HEADER_ACCOUNTING_OVERHEAD)?
            .checked_add(name.as_str().len())?
            .checked_add(value.len())?;
    }
    charge = charge.checked_add(body_bytes)?;
    for tag in tags {
        charge = charge
            .checked_add(TAG_ACCOUNTING_OVERHEAD)?
            .checked_add(tag.len())?;
    }
    Some(charge)
}
