#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const SHARDS: usize = 32;
const _: () = assert!(SHARDS.is_power_of_two() && SHARDS <= 256);

/// In-memory bounded shared cache. Representations live in shards chosen by the hash of their base request key, so
/// requests for different resources never wait for one another; the bounds of the cache are those of the whole.
#[derive(Clone)]
pub struct Cache {
    pub(super) shared: Arc<Shared>,
}

pub(super) struct Shared {
    identity: Arc<CacheIdentity>,
    pub(super) config: CacheConfig,
    clock: Arc<dyn Clock>,
    pub(super) shards: Box<[Shard]>,
    pub(super) hasher: RandomState,
    pub(super) usage: Arc<Usage>,
    rejected: AtomicU64,
    bypasses: AtomicU64,
}

#[repr(align(128))]
pub(super) struct Shard {
    pub(super) state: Mutex<State>,
}

#[derive(Debug)]
pub(super) struct CacheIdentity;

/// What the shards of a cache hold in all: the quantities the bounds of the cache are checked against, and the
/// sequence that orders their uses.
#[derive(Default)]
pub(super) struct Usage {
    pub(super) bytes: AtomicUsize,
    pub(super) entries: AtomicUsize,
    pub(super) flights: AtomicUsize,
    pub(super) sequence: AtomicU64,
}

pub(super) struct State {
    pub(super) entries: Entries,
    pub(super) flights: HashMap<BaseKey, Flight>,
    pub(super) stats: CacheStats,
    pub(super) usage: Arc<Usage>,
}

pub(super) struct Flight {
    pub(super) generation: Arc<FlightGeneration>,
    pub(super) signal: watch::Sender<FillOutcome>,
    pub(super) followers: usize,
}

pub(super) struct FlightGeneration;

enum Admission {
    Bypass,
    Lookup(RequestPolicy),
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
        let usage = Arc::new(Usage::default());
        let shards = (0..SHARDS)
            .map(|_| Shard {
                state: Mutex::new(State::new(Arc::clone(&usage))),
            })
            .collect();
        Ok(Self {
            shared: Arc::new(Shared {
                identity: Arc::new(CacheIdentity),
                config,
                clock,
                shards,
                hasher: RandomState::new(),
                usage,
                rejected: AtomicU64::new(0),
                bypasses: AtomicU64::new(0),
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
        let response = CacheResponse {
            status,
            headers,
            body,
            timing,
            tags,
        };
        self.counting_rejection(self.prepare_inner(request, response, None))
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
        self.counting_rejection(self.prepare_inner(request, response, Some(timeline)))
    }

    fn counting_rejection(
        &self,
        prepared: Result<PreparedEntry, CacheError>,
    ) -> Result<PreparedEntry, CacheError> {
        if prepared.is_err() {
            self.shared.rejected.fetch_add(1, Ordering::Relaxed);
        }
        prepared
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
            headers: owned_headers(headers),
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
        self.merge_not_modified(request, key, not_modified, timing, None)
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
        self.merge_not_modified(request, key, not_modified, timing, Some(timeline))
    }

    fn merge_not_modified(
        &self,
        request: RequestKeyInput<'_>,
        key: &CacheKey,
        not_modified: &HeaderMap,
        timing: ResponseTiming,
        timeline: Option<&CacheTimeline>,
    ) -> Result<PreparedEntry, CacheError> {
        let existing = self.stored(key).ok_or(CacheError::InvalidNotModified)?;
        let merged = merge_not_modified_headers(&existing.headers, not_modified)
            .map_err(|_| CacheError::InvalidNotModified)?;
        let tag_refs = existing.tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
        let response = CacheResponse {
            status: existing.status,
            headers: &merged,
            body: existing.body.clone(),
            timing,
            tags: &tag_refs,
        };
        let prepared = self.prepare_inner(request, response, timeline)?;
        if prepared.key != *key {
            return Err(CacheError::VaryChanged);
        }
        Ok(prepared)
    }

    fn stored(&self, key: &CacheKey) -> Option<Arc<PreparedEntry>> {
        self.shared
            .shard(key.base())
            .lock()
            .entries
            .get(key)
            .map(|stored| Arc::clone(&stored.entry))
    }

    /// Looks up one GET/HEAD request and applies request freshness constraints.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed request directives or key inputs.
    pub fn lookup(&self, request: RequestKeyInput<'_>) -> Result<Lookup, CacheError> {
        let policy = match self.admit(request)? {
            Admission::Bypass => {
                self.shared.bypasses.fetch_add(1, Ordering::Relaxed);
                return Ok(Lookup::Bypass {
                    status: LookupStatus::Bypass,
                });
            }
            Admission::Lookup(policy) => policy,
        };
        let base = BaseKey::new(request, self.shared.config.max_key_bytes)?;
        let probing = self.probing(&base, request.headers, &policy);
        let mut state = self.shared.shard(&base).lock();
        let candidate = match state.probe(&probing) {
            Probe::Absent => return Ok(cache_miss(&mut state, &policy, base)),
            Probe::Expired(key) => {
                state.remove_entry(&key);
                return Ok(cache_miss(&mut state, &policy, base));
            }
            Probe::Found(candidate) => candidate,
        };
        state.touch(candidate.entry.key());
        state.count(candidate.verdict);
        drop(state);
        Ok(candidate.into_lookup())
    }

    /// The fresh representation that answers the request as it is, and nothing else: a request or a representation
    /// that needs more than that (a bypass, a miss, a stale or revalidated entry, a malformed directive) has no answer
    /// here, so [`Cache::lookup`] can decide it. Nothing is counted or changed until the caller serves the hit and
    /// says so with [`Cache::record_hit`].
    #[must_use]
    pub fn fresh_hit(&self, request: RequestKeyInput<'_>) -> Option<FreshHit> {
        let Ok(Admission::Lookup(policy)) = self.admit(request) else {
            return None;
        };
        let base = BaseKey::new(request, self.shared.config.max_key_bytes).ok()?;
        let probing = self.probing(&base, request.headers, &policy);
        let state = self.shared.shard(&base).lock();
        let Probe::Found(candidate) = state.probe(&probing) else {
            return None;
        };
        (candidate.verdict == Verdict::Fresh).then_some(FreshHit {
            entry: candidate.entry,
            age: candidate.age,
        })
    }

    /// Counts a hit that was served and marks its representation as the most recently used.
    pub fn record_hit(&self, hit: &FreshHit) {
        let key = hit.entry.key();
        let mut state = self.shared.shard(key.base()).lock();
        state.touch(key);
        state.count(Verdict::Fresh);
    }

    fn admit(&self, request: RequestKeyInput<'_>) -> Result<Admission, CacheError> {
        if check_header_bounds(request.headers, &self.shared.config).is_err() {
            return Ok(Admission::Bypass);
        }
        let policy = RequestPolicy::evaluate(request.method, request.headers)
            .map_err(CacheError::InvalidRequest)?;
        Ok(if policy.mode == RequestMode::Bypass {
            Admission::Bypass
        } else {
            Admission::Lookup(policy)
        })
    }

    fn probing<'a>(
        &self,
        base: &'a BaseKey,
        headers: &'a HeaderMap,
        policy: &'a RequestPolicy,
    ) -> Probing<'a> {
        Probing {
            base,
            headers,
            policy,
            max_key_bytes: self.shared.config.max_key_bytes,
            authorized: headers.contains_key(http::header::AUTHORIZATION),
            now: self.shared.clock.now(),
        }
    }

    /// Records and returns an eligible stale-if-error representation by exact key.
    #[must_use]
    pub fn stale_if_error(&self, key: &CacheKey) -> Option<CachedResponse> {
        let now = self.shared.clock.now();
        let mut state = self.shared.shard(key.base()).lock();
        let entry = Arc::clone(&state.entries.get(key)?.entry);
        let age = entry_age(&entry, now);
        let staleness = age.saturating_sub(entry.policy.freshness_lifetime);
        if entry.policy.must_revalidate_stale
            || entry.policy.stale_if_error.is_zero()
            || staleness > entry.policy.stale_if_error
        {
            return None;
        }
        state.touch(key);
        state.stats.stale_if_error = state.stats.stale_if_error.saturating_add(1);
        drop(state);
        Some(response_snapshot(&entry, age))
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
        let mut state = self.shared.shard(&base).lock();
        if let Some(flight) = state.flights.get_mut(&base) {
            if flight.followers >= self.shared.config.max_followers_per_fill {
                return Ok(FillJoin::AtCapacity);
            }
            let receiver = flight.signal.subscribe();
            flight.followers += 1;
            state.stats.fill_followers = state.stats.fill_followers.saturating_add(1);
            return Ok(FillJoin::Follower(FillWaiter { receiver }));
        }
        let Some(generation) = state.begin_flight(&base, self.shared.config.max_in_flight) else {
            return Ok(FillJoin::AtCapacity);
        };
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
        let mut state = self.shared.shard(key.base()).lock();
        let mut result = PurgeResult::default();
        result.remove(&mut state, key);
        result.fills_cancelled = usize::from(state.cancel_flight(key.base(), FillOutcome::Purged));
        state.count_purged(&result);
        result
    }

    /// Purges every representation for one bounded request key and cancels its active fill.
    #[must_use]
    pub fn purge_base(&self, base: &BaseKey) -> PurgeResult {
        let mut state = self.shared.shard(base).lock();
        let keys = state.entries.variants(base).to_vec();
        let mut result = PurgeResult::default();
        for key in keys {
            result.remove(&mut state, &key);
        }
        result.fills_cancelled = usize::from(state.cancel_flight(base, FillOutcome::Purged));
        state.count_purged(&result);
        result
    }

    /// Purges entries carrying an exact bounded surrogate tag and cancels fills for affected bases.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied tag is malformed or exceeds the configured tag bound.
    pub fn purge_tag(&self, tag: &[u8]) -> Result<PurgeResult, CacheError> {
        validate_tag(tag, self.shared.config.max_tag_bytes)?;
        let mut total = PurgeResult::default();
        for shard in &self.shared.shards {
            let mut state = shard.lock();
            let keys = state.keys_tagged(tag);
            let mut result = PurgeResult::default();
            for key in keys {
                result.remove(&mut state, &key);
                result.fills_cancelled +=
                    usize::from(state.cancel_flight(key.base(), FillOutcome::Purged));
            }
            state.count_purged(&result);
            total.absorb(&result);
        }
        Ok(total)
    }

    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let mut stats = CacheStats {
            rejected: self.shared.rejected.load(Ordering::Relaxed),
            bypasses: self.shared.bypasses.load(Ordering::Relaxed),
            ..CacheStats::default()
        };
        for shard in &self.shared.shards {
            stats.absorb(&shard.lock().stats);
        }
        let usage = &self.shared.usage;
        stats.entries = usage.entries.load(Ordering::Relaxed);
        stats.bytes_used = usage.bytes.load(Ordering::Relaxed);
        stats.in_flight = usage.in_flight();
        stats
    }

    pub(crate) fn entry(&self, key: &CacheKey) -> Option<PreparedEntry> {
        self.stored(key).map(|entry| (*entry).clone())
    }

    pub(crate) fn restore(&self, entry: RecoveredEntry) -> InsertionDelta {
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
        let mut state = self.shared.shard(entry.key.base()).lock();
        let mut delta = state.insert_entry(entry);
        drop(state);
        self.shared.evict_to_bounds(&mut delta);
        delta
    }

    pub(crate) fn remove_without_cancelling_fill(&self, key: &CacheKey) -> bool {
        self.shared
            .shard(key.base())
            .lock()
            .remove_entry(key)
            .is_some()
    }

    pub(crate) fn cancel_all_fills(&self) {
        for shard in &self.shared.shards {
            shard.lock().cancel_all_flights();
        }
    }
}

pub(super) fn cache_miss(state: &mut State, policy: &RequestPolicy, base: BaseKey) -> Lookup {
    state.stats.misses = state.stats.misses.saturating_add(1);
    Lookup::Miss {
        status: LookupStatus::Miss,
        only_if_cached: policy.only_if_cached,
        base,
    }
}

/// The headers with every value in an allocation of its own. The values of a parsed response are slices of the buffer it
/// was read into (4 KiB for a response of 1 KiB, body included), and an entry that kept them would pin that buffer for
/// as long as it is cached: several times the bytes its bound charges for it.
fn owned_headers(headers: &HeaderMap) -> HeaderMap {
    let mut owned = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let mut copy = HeaderValue::from_bytes(value.as_bytes())
            .expect("the bytes of a header value are valid");
        copy.set_sensitive(value.is_sensitive());
        owned.append(name.clone(), copy);
    }
    owned
}
