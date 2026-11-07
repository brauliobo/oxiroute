use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use oxiroute_config::{PoolRegistration, Registry, UpstreamAlgorithm};
use serde::Serialize;

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
    AdministrativeState, HealthSupervisor, RoundRobinPool, RuntimeEndpoint, ServerOrigin,
    operational_event::{AuditContext, emit_server_registration},
    routing::{DynamicServer, RuntimeServer},
};

const POLL: Duration = Duration::from_secs(1);
const RATE_PER_SECOND: u32 = 20;
const RATE_BURST: u32 = 40;

/// One pool of one generation and what the registry may do to it.
pub struct PoolBinding {
    pool: Weak<RoundRobinPool>,
    name: String,
    policy: Option<PoolRegistration>,
    health: Option<HealthSupervisor>,
    protected: Arc<[SocketAddr]>,
}

impl PoolBinding {
    #[must_use]
    pub fn new(
        pool: &Arc<RoundRobinPool>,
        policy: Option<PoolRegistration>,
        health: Option<HealthSupervisor>,
        protected: Arc<[SocketAddr]>,
    ) -> Self {
        Self {
            pool: Arc::downgrade(pool),
            name: pool.name().to_owned(),
            policy,
            health,
            protected,
        }
    }

    fn live(&self) -> Option<Arc<RoundRobinPool>> {
        self.pool.upgrade()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationStatus {
    /// The server is a member of the live pool.
    Active,
    /// A static server with the same name or endpoint wins; the record is kept but inactive.
    Shadowed,
    /// The pool no longer opts into registration or no longer exists; the record is kept.
    Orphaned,
}

#[derive(Clone, Debug)]
pub struct RegistrationView {
    pub record: Registration,
    pub expires_in_ms: Option<u64>,
    pub status: RegistrationStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PutOutcome {
    pub created: bool,
    pub record: Registration,
    pub expires_in_ms: u64,
    pub registry_revision: u64,
    pub warnings: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct RegistryStatus {
    pub revision: u64,
    pub writer: bool,
    pub degraded: Option<String>,
    pub records: usize,
}

/// Counters for the metrics page.
#[derive(Clone, Debug, Default)]
pub struct RegistryMetrics {
    pub operations: BTreeMap<(&'static str, &'static str), u64>,
    pub expirations: BTreeMap<String, u64>,
    pub rejections: BTreeMap<&'static str, u64>,
    pub store_errors: u64,
    pub revision: u64,
    pub servers: BTreeMap<(String, &'static str), u64>,
}

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

    fn reconcile(binding: &PoolBinding, stored: &StoredRegistry) {
        let (Some(pool), Some(_)) = (binding.live(), &binding.policy) else {
            return;
        };
        let desired = stored
            .records
            .iter()
            .filter(|record| record.pool == binding.name)
            .collect::<Vec<_>>();
        for record in &desired {
            Self::apply(binding, &pool, record);
        }
        for member in pool.health_snapshot().endpoints {
            let stale = member.origin == ServerOrigin::Dynamic
                && !desired.iter().any(|record| record.name == member.name);
            if stale {
                Self::detach(binding, &pool, &member.name);
            }
        }
    }

    fn apply(binding: &PoolBinding, pool: &Arc<RoundRobinPool>, record: &Registration) {
        let endpoint = RuntimeEndpoint::from(record.address);
        let spec = DynamicServer {
            weight: record.weight,
            priority: record.priority,
        };
        let existing = pool.member(&record.name);
        let result = match &existing {
            Some((ServerOrigin::Static, _)) => return,
            Some((ServerOrigin::Dynamic, current)) if *current == endpoint => pool
                .update_server(&record.name, spec, record.max_connections)
                .map_err(|error| error.to_string()),
            Some((ServerOrigin::Dynamic, _)) => {
                Self::detach(binding, pool, &record.name);
                Self::join(binding, pool, record, endpoint, spec)
            }
            None if pool.member_at(&endpoint).is_some() => return,
            None => Self::join(binding, pool, record, endpoint, spec),
        };
        match result {
            Ok(()) => {
                let _ =
                    pool.set_server_administrative_state(&record.name, record.effective_state());
            }
            Err(error) => log::warn!(
                "registry: could not apply `{}` to pool `{}`: {error}",
                record.name,
                binding.name
            ),
        }
    }

    fn join(
        binding: &PoolBinding,
        pool: &Arc<RoundRobinPool>,
        record: &Registration,
        endpoint: RuntimeEndpoint,
        spec: DynamicServer,
    ) -> Result<(), String> {
        let server = RuntimeServer {
            name: record.name.clone(),
            endpoint: endpoint.clone(),
            max_connections: record.max_connections,
            pinned_addresses: None,
            protected_addresses: Arc::clone(&binding.protected),
        };
        let handle = pool
            .add_server(server, spec)
            .map_err(|error| error.to_string())?;
        if let Some(health) = &binding.health {
            health.add_target(&binding.name, handle, endpoint);
        }
        Ok(())
    }

    fn detach(binding: &PoolBinding, pool: &Arc<RoundRobinPool>, name: &str) {
        if let Ok(handle) = pool.remove_server(name)
            && let Some(health) = &binding.health
        {
            health.remove_target(&binding.name, &handle);
        }
    }

    fn apply_everywhere(state: &State, record: &Registration) {
        for binding in state
            .bindings
            .iter()
            .filter(|binding| binding.name == record.pool)
        {
            if let (Some(pool), Some(_)) = (binding.live(), &binding.policy) {
                Self::apply(binding, &pool, record);
            }
        }
    }

    fn detach_everywhere(state: &State, key: &RegistrationKey) {
        for binding in state
            .bindings
            .iter()
            .filter(|binding| binding.name == key.pool)
        {
            if let (Some(pool), Some(_)) = (binding.live(), &binding.policy)
                && pool
                    .member(&key.name)
                    .is_some_and(|(origin, _)| origin == ServerOrigin::Dynamic)
            {
                Self::detach(binding, &pool, &key.name);
            }
        }
    }

    /// The pool of the newest generation that opts into registration.
    fn registration_pool(
        state: &State,
        pool: &str,
    ) -> Result<(Arc<RoundRobinPool>, PoolRegistration, Arc<[SocketAddr]>), RegistryError> {
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
                    &stored,
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
        Ok(stored
            .records
            .into_iter()
            .filter(|record| pool.is_none_or(|pool| record.pool == pool))
            .map(|record| {
                let status = Self::status_of(&state, &record);
                let expires_in_ms = state
                    .leases
                    .remaining(&record.key(), now)
                    .map(|remaining| u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX));
                RegistrationView {
                    record,
                    expires_in_ms,
                    status,
                }
            })
            .collect())
    }

    fn status_of(state: &State, record: &Registration) -> RegistrationStatus {
        let newest = state
            .bindings
            .iter()
            .rev()
            .find(|binding| binding.name == record.pool && binding.live().is_some());
        let Some(pool) = newest
            .filter(|binding| binding.policy.is_some())
            .and_then(PoolBinding::live)
        else {
            return RegistrationStatus::Orphaned;
        };
        match pool.member(&record.name) {
            Some((ServerOrigin::Dynamic, _)) => RegistrationStatus::Active,
            _ => RegistrationStatus::Shadowed,
        }
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
        for record in &stored.records {
            let label = match Self::status_of(&state, record) {
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
            for key in state.leases.take_expired(now) {
                self.expire(&mut state, key);
            }
        } else {
            self.follow(&mut state);
        }
        state
            .leases
            .next_deadline()
            .map_or(POLL, |deadline| deadline.saturating_duration_since(now))
            .clamp(Duration::from_millis(10), POLL)
    }

    fn expire(&self, state: &mut State, key: RegistrationKey) {
        Self::detach_everywhere(state, &key);
        match self
            .store
            .update(|stored| (stored.remove(&key).is_some(), ()))
        {
            Ok((_, seq)) => state.applied_seq = seq,
            Err(error) => {
                self.store_error(state, &error);
                state.pending_removals.push(key.clone());
            }
        }
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

    fn retry_removals(&self, state: &mut State) {
        for key in std::mem::take(&mut state.pending_removals) {
            match self
                .store
                .update(|stored| (stored.remove(&key).is_some(), ()))
            {
                Ok((_, seq)) => state.applied_seq = seq,
                Err(_) => state.pending_removals.push(key),
            }
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
