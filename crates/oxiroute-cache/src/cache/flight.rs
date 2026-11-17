#[allow(clippy::wildcard_imports)]
use super::*;

impl FillGuard {
    pub(crate) fn is_current(&self) -> bool {
        self.shared.upgrade().is_some_and(|shared| {
            let state = shared.shard(&self.base).lock();
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
        Ok(match self.store_claimed(entry) {
            ClaimedStoreOutcome::Stored(delta) => StoreOutcome::Stored {
                evicted: delta.evicted,
            },
            ClaimedStoreOutcome::GenerationLost => StoreOutcome::GenerationLost,
        })
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

    pub(crate) fn store_claimed(mut self, entry: ClaimedEntry) -> ClaimedStoreOutcome {
        let Some(shared) = self.shared.upgrade() else {
            self.active = false;
            return ClaimedStoreOutcome::GenerationLost;
        };
        let mut state = shared.shard(&self.base).lock();
        if !generation_matches(&state, &self.base, &self.generation) {
            self.active = false;
            return ClaimedStoreOutcome::GenerationLost;
        }
        let flight = state.take_flight(&self.base);
        let mut delta = state.insert_entry(entry.0);
        state.stats.stores = state.stats.stores.saturating_add(1);
        if let Some(flight) = flight {
            flight.signal.send_replace(FillOutcome::Stored);
        }
        drop(state);
        shared.evict_to_bounds(&mut delta, None);
        self.active = false;
        ClaimedStoreOutcome::Stored(delta)
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
        let mut state = shared.shard(&self.base).lock();
        if !generation_matches(&state, &self.base, &self.generation) {
            return false;
        }
        if let Some(flight) = state.take_flight(&self.base) {
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
            let mut state = shared.shard(&self.base).lock();
            if generation_matches(&state, &self.base, &self.generation) {
                if let Some(flight) = state.take_flight(&self.base) {
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
    pub(super) fn shard(&self, base: &BaseKey) -> &Shard {
        let hash = self.hasher.hash_one(base);
        &self.shards[usize::from(hash.to_le_bytes()[0]) & (SHARDS - 1)]
    }

    /// Evicts the least recently used representations of the whole cache, except `keep`, until its bounds hold again,
    /// which is whether they do. The victim is the head of the shard whose head is oldest; no shard is locked while
    /// another is.
    pub(super) fn evict_to_bounds(
        &self,
        delta: &mut InsertionDelta,
        keep: Option<&CacheKey>,
    ) -> bool {
        while self.usage.exceeds(&self.config) {
            let Some(victim) = self.shards.iter().min_by_key(|shard| shard.oldest_access()) else {
                return false;
            };
            let Some(key) = victim.lock().evict_oldest(keep) else {
                return false;
            };
            delta.removed.push(key);
            delta.evicted += 1;
        }
        true
    }
}

impl Shard {
    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn oldest_access(&self) -> u64 {
        self.lock()
            .entries
            .least_recently_used()
            .map_or(u64::MAX, |(access, _)| *access)
    }
}

impl Usage {
    pub(super) fn exceeds(&self, config: &CacheConfig) -> bool {
        self.entries.load(Ordering::Relaxed) > config.max_entries
            || self.bytes.load(Ordering::Relaxed) > config.max_total_bytes
    }

    pub(super) fn in_flight(&self) -> usize {
        self.flights.load(Ordering::Relaxed)
    }

    fn reserve_flight(&self, limit: usize) -> bool {
        if self.flights.fetch_add(1, Ordering::Relaxed) >= limit {
            self.flights.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

impl State {
    pub(super) fn new(usage: Arc<Usage>) -> Self {
        Self {
            entries: Entries::default(),
            flights: HashMap::new(),
            stats: CacheStats::default(),
            usage,
        }
    }

    fn next_sequence(&self) -> u64 {
        self.usage.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(super) fn remove_entry(&mut self, key: &CacheKey) -> Option<StoredEntry> {
        let stored = self.entries.remove(key)?;
        self.usage.bytes.fetch_sub(stored.charge, Ordering::Relaxed);
        self.usage.entries.fetch_sub(1, Ordering::Relaxed);
        Some(stored)
    }

    pub(super) fn touch(&mut self, key: &CacheKey) {
        let access = self.next_sequence();
        self.entries.touch(key, access);
    }

    pub(super) fn count(&mut self, verdict: Verdict) {
        let stats = &mut self.stats;
        let counter = match verdict {
            Verdict::Fresh => &mut stats.hits,
            Verdict::Stale => &mut stats.stale_hits,
            Verdict::StaleWhileRevalidate => &mut stats.stale_while_revalidate,
            Verdict::Revalidate { .. } => &mut stats.revalidations,
        };
        *counter = counter.saturating_add(1);
    }

    pub(super) fn count_purged(&mut self, result: &PurgeResult) {
        self.stats.purged = self.stats.purged.saturating_add(result.entries as u64);
    }

    pub(super) fn keys_tagged(&self, tag: &[u8]) -> Vec<CacheKey> {
        self.entries
            .iter()
            .filter(|(_, stored)| {
                stored
                    .entry
                    .tags
                    .iter()
                    .any(|entry_tag| entry_tag.as_ref() == tag)
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    fn evict_oldest(&mut self, keep: Option<&CacheKey>) -> Option<CacheKey> {
        let key = self.entries.least_recently_used()?.1.clone();
        if keep == Some(&key) {
            return None;
        }
        self.remove_entry(&key);
        self.stats.evictions = self.stats.evictions.saturating_add(1);
        Some(key)
    }

    pub(super) fn insert_entry(&mut self, entry: PreparedEntry) -> InsertionDelta {
        let inserted = entry.key.clone();
        let mut removed = Vec::new();
        if self.remove_entry(&entry.key).is_some() {
            removed.push(entry.key.clone());
        }
        let mut conflicting = self
            .entries
            .variants(entry.key.base())
            .iter()
            .filter(|key| !key.same_vary_schema(&entry.key))
            .cloned()
            .collect::<Vec<_>>();
        conflicting.sort_unstable();
        for key in conflicting {
            if self.remove_entry(&key).is_some() {
                removed.push(key);
            }
        }
        let access = self.next_sequence();
        self.usage.bytes.fetch_add(entry.charge, Ordering::Relaxed);
        self.usage.entries.fetch_add(1, Ordering::Relaxed);
        self.entries.insert(entry, access);
        InsertionDelta {
            inserted,
            removed,
            evicted: 0,
        }
    }

    pub(super) fn begin_flight(
        &mut self,
        base: &BaseKey,
        limit: usize,
    ) -> Option<Arc<FlightGeneration>> {
        if !self.usage.reserve_flight(limit) {
            return None;
        }
        let generation = Arc::new(FlightGeneration);
        let (signal, _receiver) = watch::channel(FillOutcome::Filling);
        let flight = Flight {
            generation: Arc::clone(&generation),
            signal,
            followers: 0,
        };
        self.flights.insert(base.clone(), flight);
        Some(generation)
    }

    pub(super) fn take_flight(&mut self, base: &BaseKey) -> Option<Flight> {
        let flight = self.flights.remove(base)?;
        self.usage.flights.fetch_sub(1, Ordering::Relaxed);
        Some(flight)
    }

    pub(super) fn cancel_flight(&mut self, base: &BaseKey, outcome: FillOutcome) -> bool {
        let Some(flight) = self.take_flight(base) else {
            return false;
        };
        flight.signal.send_replace(outcome);
        self.stats.fill_cancelled = self.stats.fill_cancelled.saturating_add(1);
        true
    }

    pub(super) fn cancel_all_flights(&mut self) {
        let bases = self.flights.keys().cloned().collect::<Vec<_>>();
        for base in &bases {
            if let Some(flight) = self.take_flight(base) {
                flight.signal.send_replace(FillOutcome::Cancelled);
            }
        }
        self.stats.fill_cancelled = self.stats.fill_cancelled.saturating_add(bases.len() as u64);
    }

    pub(super) fn probe(&self, request: &Probing<'_>) -> Probe {
        let found = self
            .entries
            .variants(request.base)
            .iter()
            .filter(|key| {
                key.matches_request(request.base, request.headers, request.max_key_bytes)
                    && self.entries.get(key).is_some_and(|stored| {
                        !request.authorized || stored.entry.policy.allows_authorized_reuse
                    })
            })
            .max_by_key(|key| self.entries.get(key).map_or(0, |entry| entry.last_access));
        let Some(key) = found else {
            return Probe::Absent;
        };
        let Some(stored) = self.entries.get(key) else {
            return Probe::Absent;
        };
        let age = entry_age(&stored.entry, request.now);
        if entry_expired(&stored.entry, age) {
            return Probe::Expired(key.clone());
        }
        Probe::Found(Candidate {
            entry: Arc::clone(&stored.entry),
            age,
            verdict: Verdict::of(&stored.entry, age, request.policy),
        })
    }
}

pub(super) struct Probing<'a> {
    pub(super) base: &'a BaseKey,
    pub(super) headers: &'a HeaderMap,
    pub(super) policy: &'a RequestPolicy,
    pub(super) max_key_bytes: usize,
    pub(super) authorized: bool,
    pub(super) now: MonoTime,
}

pub(super) enum Probe {
    Absent,
    Expired(CacheKey),
    Found(Candidate),
}

pub(super) struct Candidate {
    pub(super) entry: Arc<PreparedEntry>,
    pub(super) age: Duration,
    pub(super) verdict: Verdict,
}

impl Candidate {
    pub(super) fn into_lookup(self) -> Lookup {
        let response = response_snapshot(&self.entry, self.age);
        let status = match self.verdict {
            Verdict::Fresh => LookupStatus::Hit,
            Verdict::Stale => LookupStatus::Stale,
            Verdict::StaleWhileRevalidate => LookupStatus::StaleWhileRevalidate,
            Verdict::Revalidate { stale_if_error } => {
                return Lookup::Revalidate {
                    status: LookupStatus::Revalidate,
                    response,
                    validators: self.entry.validators.clone(),
                    stale_if_error,
                };
            }
        };
        Lookup::Hit { status, response }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Verdict {
    Fresh,
    Stale,
    StaleWhileRevalidate,
    Revalidate { stale_if_error: bool },
}

impl Verdict {
    fn of(entry: &PreparedEntry, age: Duration, policy: &RequestPolicy) -> Self {
        let lifetime = entry.policy.freshness_lifetime;
        let request_fresh = policy.max_age.is_none_or(|maximum| age <= maximum);
        let fresh = age.saturating_add(policy.min_fresh) <= lifetime;
        let forced = policy.mode == RequestMode::Revalidate
            || !request_fresh
            || !policy.min_fresh.is_zero() && !fresh
            || entry.policy.always_revalidate
            || entry.policy.must_revalidate_stale && !fresh;
        if fresh && request_fresh && !forced {
            return Self::Fresh;
        }
        let staleness = age.saturating_sub(lifetime);
        let request_allows_stale = entry.policy.retention.request_stale_allowed()
            && !forced
            && !entry.policy.must_revalidate_stale
            && policy
                .max_stale
                .is_some_and(|limit| limit.is_none_or(|limit| staleness <= limit));
        if request_allows_stale {
            return Self::Stale;
        }
        let swr = !forced
            && staleness <= entry.policy.stale_while_revalidate
            && !entry.policy.stale_while_revalidate.is_zero();
        if swr {
            return Self::StaleWhileRevalidate;
        }
        Self::Revalidate {
            stale_if_error: policy.mode != RequestMode::Revalidate
                && !entry.policy.must_revalidate_stale
                && staleness <= entry.policy.stale_if_error
                && !entry.policy.stale_if_error.is_zero(),
        }
    }
}

pub(super) fn entry_age(entry: &PreparedEntry, now: MonoTime) -> Duration {
    entry
        .policy
        .corrected_initial_age
        .saturating_add(now.saturating_duration_since(entry.response_received))
}

fn entry_expired(entry: &PreparedEntry, age: Duration) -> bool {
    entry
        .policy
        .retention
        .keep()
        .is_some_and(|keep| age > entry.policy.freshness_lifetime.saturating_add(keep))
}

pub(super) fn response_snapshot(entry: &PreparedEntry, age: Duration) -> CachedResponse {
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
        variants: entry.variants.clone(),
    }
}

fn generation_matches(state: &State, base: &BaseKey, generation: &Arc<FlightGeneration>) -> bool {
    state
        .flights
        .get(base)
        .is_some_and(|flight| Arc::ptr_eq(&flight.generation, generation))
}
