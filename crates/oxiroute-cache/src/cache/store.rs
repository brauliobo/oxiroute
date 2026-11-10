#[allow(clippy::wildcard_imports)]
use super::*;

/// In-memory bounded shared cache.
#[derive(Clone)]
pub struct Cache {
    shared: Arc<Shared>,
}

pub(super) struct Shared {
    identity: Arc<CacheIdentity>,
    pub(super) config: CacheConfig,
    clock: Arc<dyn Clock>,
    pub(super) state: Mutex<State>,
}

#[derive(Debug)]
pub(super) struct CacheIdentity;

pub(super) struct State {
    pub(super) entries: Entries,
    pub(super) flights: HashMap<BaseKey, Flight>,
    pub(super) bytes_used: usize,
    pub(super) sequence: u64,
    pub(super) stats: CacheStats,
}

pub(super) struct Flight {
    pub(super) generation: Arc<FlightGeneration>,
    pub(super) signal: watch::Sender<FillOutcome>,
    followers: usize,
}

pub(super) struct FlightGeneration;

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
                    entries: Entries::default(),
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
            .touch(&key, access)
            .ok_or(CacheError::InvalidNotModified)?;
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
        state.entries.touch(key, access)?;
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
        let keys = state.entries.variants(base).to_vec();
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
        let mut state = self.shared.lock();
        insert_entry(&self.shared.config, &mut state, entry)
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
