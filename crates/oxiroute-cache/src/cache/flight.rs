#[allow(clippy::wildcard_imports)]
use super::*;

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
        let mut state = shared.lock();
        if !generation_matches(&state, &self.base, &self.generation) {
            self.active = false;
            return ClaimedStoreOutcome::GenerationLost;
        }
        let flight = state.flights.remove(&self.base);
        let delta = insert_entry(&shared.config, &mut state, entry.0);
        state.stats.stores = state.stats.stores.saturating_add(1);
        if let Some(flight) = flight {
            flight.signal.send_replace(FillOutcome::Stored);
        }
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
    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl State {
    pub(super) fn next_sequence(&mut self) -> u64 {
        if self.sequence == u64::MAX {
            self.sequence = self.entries.renumber();
        }
        self.sequence += 1;
        self.sequence
    }
}

pub(super) fn remove_expired_entry(state: &mut State, key: &CacheKey, now: MonoTime) -> bool {
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

pub(super) fn cache_miss(state: &mut State, policy: &RequestPolicy, base: BaseKey) -> Lookup {
    state.stats.misses = state.stats.misses.saturating_add(1);
    Lookup::Miss {
        status: LookupStatus::Miss,
        only_if_cached: policy.only_if_cached,
        base,
    }
}

pub(super) fn matching_key(
    state: &State,
    base: &BaseKey,
    headers: &HeaderMap,
    max_key_bytes: usize,
    authorized: bool,
) -> Option<CacheKey> {
    state
        .entries
        .variants(base)
        .iter()
        .filter(|key| {
            key.matches_request(base, headers, max_key_bytes)
                && state.entries.get(key).is_some_and(|stored| {
                    !authorized || stored.entry.policy.allows_authorized_reuse
                })
        })
        .max_by_key(|key| state.entries.get(key).map_or(0, |entry| entry.last_access))
        .cloned()
}

pub(super) fn insert_entry(
    config: &CacheConfig,
    state: &mut State,
    entry: PreparedEntry,
) -> InsertionDelta {
    let inserted = entry.key.clone();
    let mut removed = Vec::new();
    if let Some(old) = state.entries.remove(&entry.key) {
        state.bytes_used = state.bytes_used.saturating_sub(old.entry.charge);
        removed.push(entry.key.clone());
    }
    let mut conflicting = state
        .entries
        .variants(entry.key.base())
        .iter()
        .filter(|key| !key.same_vary_schema(&entry.key))
        .cloned()
        .collect::<Vec<_>>();
    conflicting.sort_unstable();
    for key in conflicting {
        if let Some(old) = state.entries.remove(&key) {
            state.bytes_used = state.bytes_used.saturating_sub(old.entry.charge);
            removed.push(key);
        }
    }
    let mut evicted = 0usize;
    while state.entries.len() >= config.max_entries
        || state.bytes_used.saturating_add(entry.charge) > config.max_total_bytes
    {
        let Some(lru) = state.entries.least_recently_used().cloned() else {
            break;
        };
        if let Some(old) = state.entries.remove(&lru) {
            state.bytes_used = state.bytes_used.saturating_sub(old.entry.charge);
            removed.push(lru);
            evicted += 1;
        }
    }
    let last_access = state.next_sequence();
    state.bytes_used = state.bytes_used.saturating_add(entry.charge);
    state.entries.insert(entry, last_access);
    state.stats.evictions = state.stats.evictions.saturating_add(evicted as u64);
    InsertionDelta {
        inserted,
        removed,
        evicted,
    }
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
    }
}

fn generation_matches(state: &State, base: &BaseKey, generation: &Arc<FlightGeneration>) -> bool {
    state
        .flights
        .get(base)
        .is_some_and(|flight| Arc::ptr_eq(&flight.generation, generation))
}

pub(super) fn cancel_flight(state: &mut State, base: &BaseKey, outcome: FillOutcome) -> bool {
    if let Some(flight) = state.flights.remove(base) {
        flight.signal.send_replace(outcome);
        state.stats.fill_cancelled = state.stats.fill_cancelled.saturating_add(1);
        true
    } else {
        false
    }
}
