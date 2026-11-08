use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use oxiroute_config::{PoolRegistration, Registry, UpstreamAlgorithm};

use super::{
    auth::{Principal, TokenStore},
    error::RegistryError,
    lease::{Clock, LeaseTable},
    limits::RateLimiter,
    record::{HeartbeatRequest, Registration, RegistrationKey, RegistrationRequest},
    store::{Store, StoreError, StoredRegistry},
    validate::{Requester, apply_heartbeat, validate_name, validate_put},
};
use crate::{
    AdministrativeState, RoundRobinPool, RuntimeEndpoint, ServerOrigin,
    observability::operational_event::{AuditContext, emit_server_registration},
};

mod reconcile;
mod view;

pub use reconcile::PoolBinding;
pub use view::{PutOutcome, RegistrationStatus, RegistrationView, RegistryMetrics, RegistryStatus};

const POLL: Duration = Duration::from_secs(1);
const RATE_PER_SECOND: u32 = 20;
const RATE_BURST: u32 = 40;

/// A registration pool of a live generation, its policy and the addresses it must never reach.
type PoolContext = (Arc<RoundRobinPool>, PoolRegistration, Arc<[SocketAddr]>);

#[derive(Default)]
struct State {
    leases: LeaseTable,
    applied_seq: u64,
    bindings: Vec<PoolBinding>,
    pending_removals: Vec<RegistrationKey>,
    degraded: Option<String>,
    metrics: RegistryMetrics,
}

/// Dynamic server registrations of one runtime process. The process that holds the writer role
/// persists and expires; the others follow the store so they never route to a departed worker.
pub struct RegistryService {
    store: Store,
    clock: Arc<dyn Clock>,
    writer: AtomicBool,
    store_errors: AtomicU64,
    tokens: Option<Arc<TokenStore>>,
    limiter: RateLimiter,
    state: Mutex<State>,
    wake: Condvar,
}

impl RegistryService {
    /// Opens the store and starts the expiry and follow thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the state directory cannot be created.
    pub fn open(registry: &Registry, clock: Arc<dyn Clock>) -> Result<Arc<Self>, StoreError> {
        let tokens = registry.tokens_file.as_ref().and_then(|path| {
            TokenStore::open(path)
                .inspect_err(|error| log::error!("registry tokens unavailable: {error}"))
                .ok()
        });
        let service = Self::with_store(Store::open(&registry.state_dir)?, clock, tokens);
        let weak = Arc::downgrade(&service);
        thread::Builder::new()
            .name("registry".into())
            .spawn(move || Self::run(&weak))
            .map_err(StoreError::Io)?;
        Ok(service)
    }

    pub(crate) fn with_store(
        store: Store,
        clock: Arc<dyn Clock>,
        tokens: Option<Arc<TokenStore>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            limiter: RateLimiter::new(Arc::clone(&clock), RATE_PER_SECOND, RATE_BURST),
            tokens,
            clock,
            writer: AtomicBool::new(false),
            store_errors: AtomicU64::new(0),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        })
    }

    /// The credential behind a scoped bearer token, if the registry has a token file.
    #[must_use]
    pub fn authenticate(&self, secret: &[u8]) -> Option<Principal> {
        self.tokens.as_ref()?.authenticate(secret)
    }

    /// Takes a rate-limit token for a caller, counting refusals.
    ///
    /// # Errors
    ///
    /// `RateLimited` with the wait until the next token.
    pub fn check_rate(&self, caller: &str) -> Result<(), RegistryError> {
        self.limiter.check(caller).map_err(|retry_after| {
            *self
                .lock()
                .metrics
                .rejections
                .entry("rate_limited")
                .or_default() += 1;
            RegistryError::RateLimited { retry_after }
        })
    }

    /// Names of the pools of live generations that accept registrations.
    #[must_use]
    pub fn registration_pools(&self) -> Vec<String> {
        let state = self.lock();
        let mut pools = state
            .bindings
            .iter()
            .filter(|binding| binding.policy.is_some() && binding.live().is_some())
            .map(|binding| binding.name.clone())
            .collect::<Vec<_>>();
        pools.sort();
        pools.dedup();
        pools
    }

    /// Members of the newest live generation's pool, static and registered.
    #[must_use]
    pub fn pool_snapshot(&self, pool: &str) -> Option<crate::PoolHealthSnapshot> {
        let state = self.lock();
        state
            .bindings
            .iter()
            .rev()
            .find(|binding| binding.name == pool)
            .and_then(PoolBinding::live)
            .map(|pool| pool.health_snapshot())
    }

    fn run(weak: &Weak<Self>) {
        while let Some(service) = weak.upgrade() {
            let wait = service.tick();
            let state = service.lock();
            drop(service.wake.wait_timeout(state, wait));
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn store_error(&self, state: &mut State, error: &StoreError) -> RegistryError {
        self.store_errors.fetch_add(1, Ordering::Relaxed);
        state.metrics.store_errors += 1;
        state.degraded = Some(error.to_string());
        log::error!("registry store: {error}");
        RegistryError::RegistryUnavailable
    }

    /// Makes this process the one that persists and expires registrations, giving every known
    /// record a full lease so a handover never expires a live worker early.
    pub fn set_writer(&self, writer: bool) {
        let mut state = self.lock();
        self.writer.store(writer, Ordering::Release);
        if writer {
            self.grant_all(&mut state);
        }
        self.wake.notify_all();
    }

    fn grant_all(&self, state: &mut State) {
        match self.store.load() {
            Ok(stored) => {
                let now = self.clock.now();
                for record in &stored.records {
                    state
                        .leases
                        .grant(record.key(), now, Duration::from_millis(record.ttl_ms));
                }
                state.degraded = None;
            }
            Err(error) => {
                self.store_error(state, &error);
            }
        }
    }

    fn require_writer(&self) -> Result<(), RegistryError> {
        if self.writer.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(RegistryError::GenerationInactive)
        }
    }

    /// Adds the pools of a generation that is about to serve and seeds them with the stored servers.
    pub fn attach(&self, bindings: Vec<PoolBinding>) {
        let mut state = self.lock();
        state.bindings.retain(|binding| binding.live().is_some());
        let first_new = state.bindings.len();
        state.bindings.extend(bindings);
        let stored = match self.store.load() {
            Ok(stored) => stored,
            Err(error) => {
                self.store_error(&mut state, &error);
                return;
            }
        };
        if self.writer.load(Ordering::Acquire) {
            let now = self.clock.now();
            for record in &stored.records {
                if state.leases.remaining(&record.key(), now).is_none() {
                    state
                        .leases
                        .grant(record.key(), now, Duration::from_millis(record.ttl_ms));
                }
            }
        }
        for binding in &state.bindings[first_new..] {
            Self::reconcile(binding, &stored);
        }
        state.applied_seq = stored.seq;
        self.wake.notify_all();
    }

    /// The pool of the newest generation that opts into registration.
    fn registration_pool(state: &State, pool: &str) -> Result<PoolContext, RegistryError> {
        let binding = state
            .bindings
            .iter()
            .rev()
            .find(|binding| binding.name == pool && binding.live().is_some())
            .ok_or(RegistryError::PoolNotFound)?;
        let policy = binding
            .policy
            .clone()
            .ok_or(RegistryError::PoolNotDynamic)?;
        Ok((
            binding.live().ok_or(RegistryError::PoolNotFound)?,
            policy,
            Arc::clone(&binding.protected),
        ))
    }

    fn count(state: &mut State, operation: &'static str, failure: Option<&RegistryError>) {
        let outcome = failure.map_or("ok", RegistryError::code);
        *state
            .metrics
            .operations
            .entry((operation, outcome))
            .or_default() += 1;
    }

    /// Creates or replaces a registration and renews its lease.
    ///
    /// # Errors
    ///
    /// Returns the refusal reason; nothing is applied unless the store accepted the change.
    pub fn put(
        &self,
        principal: &Principal,
        pool: &str,
        name: &str,
        request: RegistrationRequest,
        peer: Option<IpAddr>,
        takeover: bool,
    ) -> Result<PutOutcome, RegistryError> {
        let result = self.put_inner(principal, pool, name, request, peer, takeover);
        let mut state = self.lock();
        Self::count(&mut state, "put", result.as_ref().err());
        result
    }

    fn put_inner(
        &self,
        principal: &Principal,
        pool: &str,
        name: &str,
        request: RegistrationRequest,
        peer: Option<IpAddr>,
        takeover: bool,
    ) -> Result<PutOutcome, RegistryError> {
        self.require_writer()?;
        validate_name(name)?;
        principal.check_register(pool, name)?;
        if takeover {
            principal.check_admin()?;
        }
        let mut state = self.lock();
        let (live, policy, protected) = Self::registration_pool(&state, pool)?;
        let requester = Requester {
            id: &principal.id,
            peer,
        };
        let record = validate_put(pool, name, &policy, &protected, &requester, request)?;
        let members = live.health_snapshot().endpoints;
        let endpoint = RuntimeEndpoint::from(record.address);
        let key = record.key();
        let (verdict, seq) = self
            .store
            .update(|stored| {
                let existing = stored.get(&key).cloned();
                match Self::admit(
                    stored,
                    &members,
                    &endpoint,
                    &record,
                    existing.as_ref(),
                    &policy,
                    principal,
                    takeover,
                ) {
                    Ok(()) => {
                        let mut record = record.clone();
                        if let Some(existing) = &existing {
                            record.operator_state = existing.operator_state;
                        }
                        let changed = stored.upsert(record.clone());
                        (changed, Ok((existing.is_none(), changed, record)))
                    }
                    Err(error) => (false, Err(error)),
                }
            })
            .map_err(|error| self.store_error(&mut state, &error))?;
        let (created, changed, record) = verdict?;
        state
            .leases
            .grant(key, self.clock.now(), Duration::from_millis(record.ttl_ms));
        state.applied_seq = seq;
        Self::apply_everywhere(&state, &record);
        let warnings = Self::warnings(&live, &record);
        log::info!(
            "registry: {} `{}` in pool `{}`",
            if created { "registered" } else { "updated" },
            record.name,
            record.pool
        );
        if changed {
            announce(&principal.id);
        }
        self.wake.notify_all();
        Ok(PutOutcome {
            created,
            expires_in_ms: record.ttl_ms,
            record,
            registry_revision: seq,
            warnings,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn admit(
        stored: &StoredRegistry,
        members: &[crate::EndpointHealthSnapshot],
        endpoint: &RuntimeEndpoint,
        record: &Registration,
        existing: Option<&Registration>,
        policy: &PoolRegistration,
        principal: &Principal,
        takeover: bool,
    ) -> Result<(), RegistryError> {
        if members
            .iter()
            .any(|member| member.origin == ServerOrigin::Static && member.name == record.name)
        {
            return Err(RegistryError::NameConflict);
        }
        let address_taken = members
            .iter()
            .any(|member| member.address == *endpoint && member.name != record.name)
            || stored.records.iter().any(|other| {
                other.pool == record.pool
                    && other.address == record.address
                    && other.name != record.name
            });
        if address_taken {
            return Err(RegistryError::EndpointConflict);
        }
        if let Some(existing) = existing
            && existing.address != record.address
            && existing.registered_by != principal.id
            && !takeover
        {
            return Err(RegistryError::LeaseHeld);
        }
        let others = stored
            .records
            .iter()
            .filter(|other| other.pool == record.pool && other.name != record.name)
            .count();
        if others >= usize::from(policy.max_servers) {
            return Err(RegistryError::PoolFull);
        }
        Ok(())
    }

    fn warnings(pool: &Arc<RoundRobinPool>, record: &Registration) -> Vec<&'static str> {
        let ignored = record.weight != 1
            && !matches!(
                pool.algorithm(),
                UpstreamAlgorithm::WeightedRoundRobin { .. }
            );
        if ignored {
            vec!["weight_ignored"]
        } else {
            Vec::new()
        }
    }

    /// Renews a lease, optionally announcing a state change. Heartbeats persist only when a field changed.
    ///
    /// # Errors
    ///
    /// `RegistrationNotFound` means the worker must register again.
    pub fn heartbeat(
        &self,
        principal: &Principal,
        pool: &str,
        name: &str,
        request: HeartbeatRequest,
    ) -> Result<u64, RegistryError> {
        self.require_writer()?;
        principal.check_register(pool, name)?;
        let mut state = self.lock();
        let (_, policy, _) = Self::registration_pool(&state, pool)?;
        let key = RegistrationKey::new(pool, name);
        let (verdict, seq) = self
            .store
            .update(|stored| {
                let Some(existing) = stored.get(&key).cloned() else {
                    return (false, Err(RegistryError::RegistrationNotFound));
                };
                if existing.registered_by != principal.id && principal.check_admin().is_err() {
                    return (false, Err(RegistryError::LeaseHeld));
                }
                match apply_heartbeat(&policy, existing, request) {
                    Ok(record) => (stored.upsert(record.clone()), Ok(record)),
                    Err(error) => (false, Err(error)),
                }
            })
            .map_err(|error| self.store_error(&mut state, &error))?;
        let record = verdict?;
        state
            .leases
            .grant(key, self.clock.now(), Duration::from_millis(record.ttl_ms));
        state.applied_seq = state.applied_seq.max(seq);
        Self::apply_everywhere(&state, &record);
        *state
            .metrics
            .operations
            .entry(("heartbeat", "ok"))
            .or_default() += 1;
        Ok(record.ttl_ms)
    }

    /// Removes a registration.
    ///
    /// # Errors
    ///
    /// `RegistrationNotFound` when the name is not registered.
    pub fn delete(
        &self,
        principal: &Principal,
        pool: &str,
        name: &str,
    ) -> Result<u64, RegistryError> {
        self.require_writer()?;
        principal.check_register(pool, name)?;
        let mut state = self.lock();
        let key = RegistrationKey::new(pool, name);
        let (verdict, seq) = self
            .store
            .update(|stored| match stored.get(&key) {
                None => (false, Err(RegistryError::RegistrationNotFound)),
                Some(existing)
                    if existing.registered_by != principal.id
                        && principal.check_admin().is_err() =>
                {
                    (false, Err(RegistryError::LeaseHeld))
                }
                Some(_) => (stored.remove(&key).is_some(), Ok(())),
            })
            .map_err(|error| self.store_error(&mut state, &error))?;
        verdict?;
        state.leases.remove(&key);
        state.applied_seq = seq;
        Self::detach_everywhere(&state, &key);
        *state
            .metrics
            .operations
            .entry(("delete", "ok"))
            .or_default() += 1;
        log::info!("registry: deregistered `{name}` from pool `{pool}`");
        announce(&principal.id);
        self.wake.notify_all();
        Ok(seq)
    }

    /// Sets the operator's drain or maintenance decision, which outlives re-registration.
    ///
    /// # Errors
    ///
    /// `RegistrationNotFound` when the name is not registered.
    pub fn set_operator_state(
        &self,
        principal: &Principal,
        pool: &str,
        name: &str,
        operator_state: AdministrativeState,
    ) -> Result<u64, RegistryError> {
        self.require_writer()?;
        principal.check_admin()?;
        let mut state = self.lock();
        let key = RegistrationKey::new(pool, name);
        let (verdict, seq) = self
            .store
            .update(|stored| {
                let Some(mut record) = stored.get(&key).cloned() else {
                    return (false, Err(RegistryError::RegistrationNotFound));
                };
                record.operator_state = operator_state;
                (stored.upsert(record.clone()), Ok(record))
            })
            .map_err(|error| self.store_error(&mut state, &error))?;
        let record = verdict?;
        state.applied_seq = seq;
        Self::apply_everywhere(&state, &record);
        announce(&principal.id);
        Ok(seq)
    }

    /// All registrations, optionally of one pool, with lease remainders and live status.
    ///
    /// # Errors
    ///
    /// `RegistryUnavailable` when the store cannot be read.
    pub fn list(&self, pool: Option<&str>) -> Result<Vec<RegistrationView>, RegistryError> {
        let mut state = self.lock();
        let stored = self
            .store
            .load()
            .map_err(|error| self.store_error(&mut state, &error))?;
        let now = self.clock.now();
        let records = stored
            .records
            .iter()
            .filter(|record| pool.is_none_or(|pool| record.pool == pool))
            .collect::<Vec<_>>();
        Ok(Self::statuses(&state, &records)
            .into_iter()
            .zip(records)
            .map(|(status, record)| {
                let expires_in_ms = state
                    .leases
                    .remaining(&record.key(), now)
                    .map(|remaining| u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX));
                RegistrationView {
                    record: record.clone(),
                    expires_in_ms,
                    status,
                }
            })
            .collect())
    }

    /// The status of each record, looking at every pool once however many records it holds.
    fn statuses(state: &State, records: &[&Registration]) -> Vec<RegistrationStatus> {
        let mut pools: HashMap<&str, Option<HashMap<String, ServerOrigin>>> = HashMap::new();
        records
            .iter()
            .map(|record| {
                let origins = pools.entry(record.pool.as_str()).or_insert_with(|| {
                    state
                        .bindings
                        .iter()
                        .rev()
                        .find(|binding| binding.name == record.pool && binding.live().is_some())
                        .filter(|binding| binding.policy.is_some())
                        .and_then(PoolBinding::live)
                        .map(|pool| pool.member_origins())
                });
                match origins.as_ref().map(|origins| origins.get(&record.name)) {
                    None => RegistrationStatus::Orphaned,
                    Some(Some(ServerOrigin::Dynamic)) => RegistrationStatus::Active,
                    Some(_) => RegistrationStatus::Shadowed,
                }
            })
            .collect()
    }

    /// Store revision, role and health of the registry.
    #[must_use]
    pub fn status(&self) -> RegistryStatus {
        let state = self.lock();
        let stored = self.store.load().unwrap_or_default();
        RegistryStatus {
            revision: stored.seq,
            writer: self.writer.load(Ordering::Acquire),
            degraded: state.degraded.clone(),
            records: stored.records.len(),
        }
    }

    /// A copy of the counters, with current server counts per pool and state.
    #[must_use]
    pub fn metrics(&self) -> RegistryMetrics {
        let state = self.lock();
        let mut metrics = state.metrics.clone();
        let stored = self.store.load().unwrap_or_default();
        metrics.revision = stored.seq;
        let now = self.clock.now();
        let records = stored.records.iter().collect::<Vec<_>>();
        let statuses = Self::statuses(&state, &records);
        for (record, status) in records.into_iter().zip(statuses) {
            if let Some(left) = state.leases.remaining(&record.key(), now) {
                metrics.lease_remaining_ms.insert(
                    (record.pool.clone(), record.name.clone()),
                    u64::try_from(left.as_millis()).unwrap_or(u64::MAX),
                );
            }
            let label = match status {
                RegistrationStatus::Active => "active",
                RegistrationStatus::Shadowed => "shadowed",
                RegistrationStatus::Orphaned => "orphaned",
            };
            *metrics
                .servers
                .entry((record.pool.clone(), label))
                .or_default() += 1;
        }
        metrics
    }

    /// Expires overdue leases (writer) or follows the store (other processes). Returns how long
    /// to wait before the next useful call.
    pub fn tick(&self) -> Duration {
        let mut state = self.lock();
        let now = self.clock.now();
        if self.writer.load(Ordering::Acquire) {
            self.retry_removals(&mut state);
            let expired = state.leases.take_expired(now);
            self.expire(&mut state, &expired);
        } else {
            self.follow(&mut state);
        }
        state
            .leases
            .next_deadline()
            .map_or(POLL, |deadline| deadline.saturating_duration_since(now))
            .clamp(Duration::from_millis(10), POLL)
    }

    /// Detaches the registrations whose lease ran out and removes them from the store in one write.
    fn expire(&self, state: &mut State, keys: &[RegistrationKey]) {
        if keys.is_empty() {
            return;
        }
        for key in keys {
            Self::detach_everywhere(state, key);
        }
        self.forget(state, keys);
        for key in keys {
            *state
                .metrics
                .expirations
                .entry(key.pool.clone())
                .or_default() += 1;
            log::info!(
                "registry: lease of `{}` in pool `{}` expired",
                key.name,
                key.pool
            );
            announce("lease_expiry");
        }
    }

    /// Removes the records from the store; those it cannot remove now are retried by the next tick.
    fn forget(&self, state: &mut State, keys: &[RegistrationKey]) {
        match self
            .store
            .update(|stored| (stored.remove_all(keys) > 0, ()))
        {
            Ok(((), seq)) => state.applied_seq = seq,
            Err(error) => {
                self.store_error(state, &error);
                state.pending_removals.extend_from_slice(keys);
            }
        }
    }

    fn retry_removals(&self, state: &mut State) {
        let pending = std::mem::take(&mut state.pending_removals);
        if !pending.is_empty() {
            self.forget(state, &pending);
        }
    }

    fn follow(&self, state: &mut State) {
        let stored = match self.store.load() {
            Ok(stored) => stored,
            Err(error) => {
                self.store_error(state, &error);
                return;
            }
        };
        if stored.seq == state.applied_seq {
            return;
        }
        for binding in &state.bindings {
            Self::reconcile(binding, &stored);
        }
        state.applied_seq = stored.seq;
        state.degraded = None;
    }
}

/// Reports a membership change as the operational `server_update` event.
fn announce(actor: &str) {
    emit_server_registration(&AuditContext::registry(actor));
}

#[cfg(test)]
mod tests;
