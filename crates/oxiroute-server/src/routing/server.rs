#[allow(clippy::wildcard_imports)]
use super::*;

const LOAD_SCALE: u64 = 1000;

#[derive(Clone, Debug)]
pub(crate) struct RuntimeServer {
    pub(crate) name: String,
    pub(crate) endpoint: RuntimeEndpoint,
    pub(crate) max_connections: Option<u64>,
    pub(crate) pinned_addresses: Option<Arc<[SocketAddr]>>,
    pub(crate) protected_addresses: Arc<[SocketAddr]>,
}

/// Why a server may take work: it is selectable, or its pool is failing open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Admission {
    Selectable,
    FailOpen,
}

impl Admission {
    fn admits(self, server: &PoolEndpoint, now_unix_ms: u64) -> bool {
        match self {
            Self::Selectable => server.selectable_at(now_unix_ms),
            Self::FailOpen => server.fail_open_eligible(),
        }
    }
}

/// Where a pool member comes from: the configuration file or a runtime registration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerOrigin {
    Static,
    Dynamic,
}

/// A registered server about to join a pool, with the administrative state it joins in.
#[derive(Debug)]
pub(crate) struct NewMember {
    pub(crate) server: RuntimeServer,
    pub(crate) spec: DynamicServer,
    pub(crate) state: AdministrativeState,
}

/// Placement and balancing weight of a server that joins a pool at runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DynamicServer {
    pub(crate) weight: u16,
    /// Dynamic servers follow the static ones, ordered by priority and then by name.
    pub(crate) priority: u16,
}

/// The servers of a pool, in rotation order. Replaced as a whole.
#[derive(Debug)]
pub(super) struct Membership {
    pub(super) endpoints: Box<[Arc<PoolEndpoint>]>,
    names: NameIndex,
    /// The members that may be chosen; refreshed by [`PoolHealthState::selectable_view`].
    pub(super) selectable: ArcSwap<SelectableView>,
}

impl Membership {
    pub(super) fn new(endpoints: Vec<Arc<PoolEndpoint>>) -> Self {
        for (slot, server) in (0..).zip(&endpoints) {
            server.slot.store(slot, Ordering::Relaxed);
        }
        // Pairs with the fence in `LoadLog::note`: a change whose note read the old slot is
        // visible to the index that is built for this membership.
        fence(Ordering::SeqCst);
        Self {
            names: NameIndex::new(&endpoints),
            endpoints: endpoints.into_boxed_slice(),
            selectable: ArcSwap::from_pointee(SelectableView::unbuilt()),
        }
    }

    pub(super) fn position(&self, name: &str) -> Option<usize> {
        self.names.find(&self.endpoints, name)
    }

    /// Where a dynamic server belongs: after every static one, by priority and then name.
    pub(super) fn dynamic_position(
        endpoints: &[Arc<PoolEndpoint>],
        priority: u16,
        name: &str,
    ) -> usize {
        let statics = endpoints
            .iter()
            .take_while(|server| server.origin == ServerOrigin::Static)
            .count();
        statics
            + endpoints[statics..].partition_point(|server| {
                (server.priority(), server.name.as_str()) < (priority, name)
            })
    }
}

/// Why a server could not join, change or leave a pool.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum PoolMembershipError {
    #[error("the pool already has a server named `{0}`")]
    DuplicateName(String),
    #[error("the pool has no server named `{0}`")]
    UnknownServer(String),
    #[error(transparent)]
    Invalid(#[from] PoolError),
}

/// Addresses one server of a pool without a position, so membership can change under it.
#[derive(Clone, Debug)]
pub(crate) struct ServerHandle(pub(super) Arc<PoolEndpoint>);

impl PartialEq for ServerHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl ServerHandle {
    pub(crate) fn state(&self) -> EndpointHealthState {
        self.0.state()
    }

    pub(crate) fn checks_running(&self) -> bool {
        self.0.checks_running()
    }

    pub(crate) fn passively_ejected(&self) -> bool {
        self.0.passively_ejected_at(now_unix_ms())
    }
}

#[derive(Debug)]
pub(super) struct PoolEndpoint {
    pub(super) active_work: AtomicU64,
    /// Pooled keep-alive connections that hold a permit but are not serving a request.
    pub(super) idle_permits: AtomicU64,
    /// Idle connections promised to selected requests that have not reused them yet.
    pub(super) reserved_idle: AtomicU64,
    /// The position in the current membership, kept by [`Membership::new`].
    pub(super) slot: AtomicU32,
    pub(super) breaker: Breaker,
    breaker_policy: Option<CircuitBreakerPolicy>,
    pub(super) administrative_state: AtomicU8,
    pub(super) checks_enabled: AtomicBool,
    pub(super) health_override: AtomicU8,
    pub(super) endpoint: RuntimeEndpoint,
    /// Counters outlive generations; resolved once here instead of on every request.
    pub(super) stats: Arc<ServerStats>,
    /// Names this server in a sticky-session cookie; hashed on first use.
    pub(super) cookie_id: std::sync::OnceLock<u64>,
    /// Zero means unlimited; registered servers may change it while they are members.
    pub(super) configured_max_connections: AtomicU64,
    pub(super) max_connections_override: AtomicU64,
    pub(super) name: String,
    pinned_addresses: Option<RwLock<Arc<[SocketAddr]>>>,
    protected_addresses: Arc<[SocketAddr]>,
    /// The shared lookup of a DNS endpoint that resolves on connect.
    pub(super) resolved: ResolvedAddresses,
    pub(super) origin: ServerOrigin,
    pub(super) priority: AtomicU16,
    pub(super) state: AtomicU8,
    configured_weight: AtomicU16,
    pub(super) weight_override: AtomicU16,
    pub(super) last_checked_at_unix_ms: AtomicU64,
    pub(super) last_transition_at_unix_ms: AtomicU64,
    pub(super) successful_checks: AtomicU64,
    pub(super) failed_checks: AtomicU64,
    pub(super) consecutive_successes: AtomicU64,
    pub(super) consecutive_failures: AtomicU64,
    last_failure: AtomicU8,
    pub(super) passive_ejected: AtomicBool,
    /// An active probe, not the clock, ends the current ejection.
    awaiting_probe: AtomicBool,
    pub(super) probing: AtomicBool,
    pub(super) passive_failure_count: AtomicU64,
    passive_consecutive_failures: AtomicU64,
    passive_ejection_count: AtomicU64,
    passive_backoff_step: AtomicU64,
    passive_last_failure: AtomicU8,
    passive_ejected_at_unix_ms: AtomicU64,
    pub(super) passive_ejection_until_unix_ms: AtomicU64,
    passive_last_recovery_at_unix_ms: AtomicU64,
    passive_recovery_count: AtomicU64,
}

impl PoolEndpoint {
    pub(super) fn new(
        pool: &str,
        server: RuntimeServer,
        startup: Option<HealthStartup>,
        weight: u16,
        origin: ServerOrigin,
        priority: u16,
        breaker_policy: Option<CircuitBreakerPolicy>,
    ) -> Self {
        let state = match startup {
            None => EndpointHealthState::Unchecked,
            Some(HealthStartup::Healthy) => EndpointHealthState::Healthy,
            Some(HealthStartup::Unhealthy) => EndpointHealthState::Unhealthy,
            Some(HealthStartup::Checking) => EndpointHealthState::Unknown,
        };
        Self {
            active_work: AtomicU64::new(0),
            idle_permits: AtomicU64::new(0),
            reserved_idle: AtomicU64::new(0),
            slot: AtomicU32::new(0),
            breaker: Breaker::default(),
            breaker_policy,
            administrative_state: AtomicU8::new(AdministrativeState::Ready as u8),
            checks_enabled: AtomicBool::new(true),
            health_override: AtomicU8::new(HealthOverride::Auto as u8),
            endpoint: server.endpoint,
            stats: upstream_stats().server(pool, &server.name),
            cookie_id: std::sync::OnceLock::new(),
            configured_max_connections: AtomicU64::new(server.max_connections.unwrap_or(0)),
            max_connections_override: AtomicU64::new(0),
            name: server.name,
            pinned_addresses: server.pinned_addresses.map(RwLock::new),
            protected_addresses: server.protected_addresses,
            resolved: ResolvedAddresses::default(),
            origin,
            priority: AtomicU16::new(priority),
            state: AtomicU8::new(state as u8),
            configured_weight: AtomicU16::new(weight),
            weight_override: AtomicU16::new(0),
            last_checked_at_unix_ms: AtomicU64::new(0),
            last_transition_at_unix_ms: AtomicU64::new(0),
            successful_checks: AtomicU64::new(0),
            failed_checks: AtomicU64::new(0),
            consecutive_successes: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            last_failure: AtomicU8::new(0),
            passive_ejected: AtomicBool::new(false),
            awaiting_probe: AtomicBool::new(false),
            probing: AtomicBool::new(false),
            passive_failure_count: AtomicU64::new(0),
            passive_consecutive_failures: AtomicU64::new(0),
            passive_ejection_count: AtomicU64::new(0),
            passive_backoff_step: AtomicU64::new(0),
            passive_last_failure: AtomicU8::new(0),
            passive_ejected_at_unix_ms: AtomicU64::new(0),
            passive_ejection_until_unix_ms: AtomicU64::new(0),
            passive_last_recovery_at_unix_ms: AtomicU64::new(0),
            passive_recovery_count: AtomicU64::new(0),
        }
    }

    pub(super) fn state(&self) -> EndpointHealthState {
        EndpointHealthState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub(super) fn selectable_at(&self, now_unix_ms: u64) -> bool {
        self.administrative_state() == AdministrativeState::Ready
            && !self.passively_ejected_at(now_unix_ms)
            && self.breaker_permits(now_unix_ms)
            && self.health_permits()
    }

    /// The first time at which time alone may make this server selectable again, or `u64::MAX`.
    pub(super) fn selectable_after(&self, now_unix_ms: u64) -> u64 {
        let ejection_ends = (self.passive_ejected.load(Ordering::Acquire)
            && !self.awaiting_probe.load(Ordering::Acquire))
        .then(|| self.passive_ejection_until_unix_ms.load(Ordering::Acquire))
        .filter(|until| *until > now_unix_ms);
        let breaker_closes = self.breaker.reopens_at().filter(|at| *at > now_unix_ms);
        ejection_ends
            .into_iter()
            .chain(breaker_closes)
            .min()
            .unwrap_or(u64::MAX)
    }

    fn health_permits(&self) -> bool {
        match self.health_override() {
            HealthOverride::Auto => self.state().selectable(),
            HealthOverride::Up => true,
            HealthOverride::Down => false,
        }
    }

    /// Administrative drain, maintenance and a forced-down override stay honoured while failing open.
    pub(super) fn fail_open_eligible(&self) -> bool {
        self.administrative_state() == AdministrativeState::Ready
            && self.health_override() != HealthOverride::Down
    }

    /// Orders fail-open candidates: healthy but ejected soonest first, then the unhealthy with the
    /// fewest consecutive failed checks.
    pub(super) fn fail_open_rank(&self) -> (bool, u64, u64) {
        let ejected_until = self.passive_ejection_until_unix_ms.load(Ordering::Acquire);
        if self.health_permits() {
            (false, ejected_until, 0)
        } else {
            (
                true,
                self.consecutive_failures.load(Ordering::Acquire),
                ejected_until,
            )
        }
    }

    pub(super) fn admission(&self, now_unix_ms: u64, failing_open: bool) -> Option<Admission> {
        if self.selectable_at(now_unix_ms) {
            Some(Admission::Selectable)
        } else {
            (failing_open && self.fail_open_eligible()).then_some(Admission::FailOpen)
        }
    }

    pub(super) fn passively_ejected_at(&self, now_unix_ms: u64) -> bool {
        self.passive_ejected.load(Ordering::Acquire)
            && (self.awaiting_probe.load(Ordering::Acquire)
                || self.passive_ejection_until_unix_ms.load(Ordering::Acquire) > now_unix_ms)
    }

    #[inline]
    pub(super) fn has_capacity(&self) -> bool {
        self.max_connections()
            .is_none_or(|limit| self.active_work.load(Ordering::Acquire) < limit)
    }

    /// The load when the server can take work now, from one read of its counters; with
    /// `reuse_idle` an idle pooled connection also qualifies, since it needs no new permit.
    #[inline]
    pub(super) fn load_if_open(&self, reuse_idle: bool) -> Option<u64> {
        let limit = self.configured_limit();
        let active = self.active_work.load(Ordering::Acquire);
        let open = self.admitting_limit(limit).is_none_or(|max| active < max)
            || (reuse_idle && self.has_idle_connection());
        open.then(|| self.load_of(active, limit))
    }

    pub(super) fn configured_weight(&self) -> u16 {
        self.configured_weight.load(Ordering::Acquire)
    }

    pub(super) fn set_configured_weight(&self, weight: u16) {
        self.configured_weight.store(weight, Ordering::Release);
    }

    #[inline]
    pub(super) fn weight(&self) -> u16 {
        match self.weight_override.load(Ordering::Acquire) {
            0 => self.configured_weight(),
            weight => weight,
        }
    }

    /// Work in flight per unit of capacity, so servers of different size compare fairly.
    pub(super) fn load(&self) -> u64 {
        self.load_of(
            self.active_work.load(Ordering::Acquire),
            self.configured_limit(),
        )
    }

    #[inline]
    fn load_of(&self, active: u64, limit: Option<u64>) -> u64 {
        let divisor = limit.unwrap_or_else(|| u64::from(self.weight())).max(1);
        active.saturating_mul(LOAD_SCALE) / divisor
    }

    /// Whether a pooled connection waits idle that no selected request has claimed.
    #[inline]
    pub(super) fn has_idle_connection(&self) -> bool {
        self.idle_permits.load(Ordering::Acquire) > self.reserved_idle.load(Ordering::Acquire)
    }

    /// Claims an idle pooled connection for a request that was just selected, so the requests
    /// selected after it see the server busy instead of binding to the same connection.
    pub(super) fn reserve_idle(&self, queue: &PoolQueue) -> bool {
        let reserved = self
            .reserved_idle
            .try_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
                (reserved < self.idle_permits.load(Ordering::Acquire)).then_some(reserved + 1)
            })
            .is_ok();
        if reserved {
            queue.load_log.note(self);
        }
        reserved
    }

    pub(super) fn release_idle(&self, queue: &PoolQueue) {
        let released =
            self.reserved_idle
                .try_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
                    reserved.checked_sub(1)
                });
        debug_assert!(released.is_ok(), "idle reservation counter underflow");
        queue.load_log.note(self);
    }

    /// Counts a pooled connection of this server as idle or in use again.
    pub(super) fn set_connection_idle(&self, queue: &PoolQueue, idle: bool) {
        if idle {
            self.idle_permits.fetch_add(1, Ordering::AcqRel);
        } else {
            self.idle_permits.fetch_sub(1, Ordering::AcqRel);
        }
        queue.load_log.note(self);
    }

    pub(super) fn administrative_state(&self) -> AdministrativeState {
        AdministrativeState::from_u8(self.administrative_state.load(Ordering::Acquire))
    }

    pub(super) fn health_override(&self) -> HealthOverride {
        HealthOverride::from_u8(self.health_override.load(Ordering::Acquire))
    }

    fn configured_max_connections(&self) -> Option<u64> {
        nonzero(self.configured_max_connections.load(Ordering::Acquire))
    }

    pub(super) fn priority(&self) -> u16 {
        self.priority.load(Ordering::Acquire)
    }

    /// The operator's limit: the runtime override, else the configured one.
    #[inline]
    fn configured_limit(&self) -> Option<u64> {
        match self.max_connections_override.load(Ordering::Acquire) {
            0 => self.configured_max_connections(),
            limit => Some(limit),
        }
    }

    /// The limit that admits work now: a half-open breaker allows only its trial requests.
    fn max_connections(&self) -> Option<u64> {
        self.admitting_limit(self.configured_limit())
    }

    #[inline]
    fn admitting_limit(&self, limit: Option<u64>) -> Option<u64> {
        match self.breaker_policy {
            Some(policy) if self.breaker.is_half_open() => {
                Some(limit.map_or(policy.half_open_max(), |limit| {
                    limit.min(policy.half_open_max())
                }))
            }
            _ => limit,
        }
    }

    fn breaker_permits(&self, now_unix_ms: u64) -> bool {
        self.breaker_policy.is_none() || self.breaker.permits(now_unix_ms)
    }

    pub(super) fn checks_running(&self) -> bool {
        self.checks_enabled.load(Ordering::Acquire)
            && self.administrative_state() != AdministrativeState::Maintenance
    }

    pub(super) fn try_acquire(
        self: &Arc<Self>,
        queue: &Arc<PoolQueue>,
        health: &Arc<PoolHealthState>,
        admission: Admission,
        now_unix_ms: u64,
    ) -> Option<EndpointLease> {
        self.try_acquire_permit(queue, admission, now_unix_ms)
            .then(|| {
                EndpointLease::acquired(Arc::clone(self), Arc::clone(queue), Arc::clone(health))
            })
    }

    /// Takes one capacity permit when `admission` allows this server; the caller owns it.
    pub(super) fn try_acquire_permit(
        &self,
        queue: &PoolQueue,
        admission: Admission,
        now_unix_ms: u64,
    ) -> bool {
        if !admission.admits(self, now_unix_ms)
            || self
                .try_acquire_capacity(queue, admission, now_unix_ms)
                .is_none()
        {
            return false;
        }
        if admission.admits(self, now_unix_ms) {
            return true;
        }
        self.release_capacity(queue);
        false
    }

    pub(super) fn try_acquire_capacity(
        &self,
        queue: &PoolQueue,
        admission: Admission,
        now_unix_ms: u64,
    ) -> Option<()> {
        self.active_work
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                if !admission.admits(self, now_unix_ms)
                    || self.max_connections().is_some_and(|limit| active >= limit)
                {
                    None
                } else {
                    active.checked_add(1)
                }
            })
            .ok()?;
        queue.load_log.note(self);
        Some(())
    }

    pub(super) fn release_capacity(&self, queue: &PoolQueue) {
        let released = self
            .active_work
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            });
        debug_assert!(released.is_ok(), "endpoint lease counter underflow");
        queue.load_log.note(self);
        queue.notify_capacity_waiters();
    }

    /// The addresses to connect to: the pinned ones, else the lookup shared with the other requests.
    pub(super) async fn resolve_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        if let Some(addresses) = self.pinned() {
            self.reject_protected_addresses(&addresses)?;
            return Ok(addresses);
        }
        if matches!(self.endpoint, RuntimeEndpoint::Dns { .. }) {
            let shared = self.resolved.get_or_resolve(|| self.lookup()).await?;
            return Ok(shared.to_vec());
        }
        let addresses = self.endpoint.resolve_addresses().await?;
        self.reject_protected_addresses(&addresses)?;
        Ok(addresses)
    }

    /// Like [`Self::resolve_addresses`], but a DNS endpoint is looked up again and the shared
    /// lookup takes the answer, as for a health probe.
    pub(super) async fn resolve_probe_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        if self.pinned_addresses.is_some() || !matches!(self.endpoint, RuntimeEndpoint::Dns { .. })
        {
            return self.resolve_addresses().await;
        }
        let addresses = self.lookup().await?;
        self.resolved.store(&addresses);
        Ok(addresses.to_vec())
    }

    /// Forgets the shared lookup after a connection to the endpoint failed, since its addresses may
    /// have changed.
    pub(super) fn forget_resolution(&self) {
        self.resolved.invalidate();
    }

    fn pinned(&self) -> Option<Vec<SocketAddr>> {
        self.pinned_addresses.as_ref().map(|addresses| {
            addresses
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .to_vec()
        })
    }

    async fn lookup(&self) -> io::Result<Arc<[SocketAddr]>> {
        let addresses = self.endpoint.resolve_addresses().await?;
        self.reject_protected_addresses(&addresses)?;
        Ok(addresses.into())
    }

    pub(super) async fn resolve_fresh_dns(&self) -> io::Result<Vec<SocketAddr>> {
        if !matches!(self.endpoint, RuntimeEndpoint::Dns { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "upstream server does not use DNS",
            ));
        }
        let addresses = self.endpoint.resolve_addresses().await?;
        self.reject_protected_addresses(&addresses)?;
        Ok(addresses)
    }

    pub(super) fn commit_dns(&self, addresses: &[SocketAddr]) -> io::Result<()> {
        if !matches!(self.endpoint, RuntimeEndpoint::Dns { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "upstream server does not use DNS",
            ));
        }
        self.reject_protected_addresses(addresses)?;
        if let Some(pinned) = &self.pinned_addresses {
            *pinned
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = addresses.to_vec().into();
        }
        Ok(())
    }

    fn reject_protected_addresses(&self, addresses: &[SocketAddr]) -> io::Result<()> {
        if addresses.iter().any(|address| {
            self.protected_addresses.iter().any(|protected| {
                address.port() == protected.port()
                    && (address.ip() == protected.ip() || protected.ip().is_unspecified())
            })
        }) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "upstream DNS resolved to a protected management or statistics listener",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn record(
        &self,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
        passive_policy: PassiveFailurePolicy,
    ) -> HealthUpdate {
        if let Some(at_unix_ms) = at_unix_ms {
            self.last_checked_at_unix_ms
                .store(at_unix_ms, Ordering::Relaxed);
        }
        let previous = self.state();
        let (next, consecutive) = if healthy {
            self.successful_checks.fetch_add(1, Ordering::Relaxed);
            let consecutive = self
                .consecutive_successes
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            self.consecutive_failures.store(0, Ordering::Relaxed);
            self.last_failure.store(0, Ordering::Relaxed);
            let next = if matches!(
                previous,
                EndpointHealthState::Unknown | EndpointHealthState::Unhealthy
            ) && consecutive >= u64::from(healthy_threshold)
            {
                EndpointHealthState::Healthy
            } else {
                previous
            };
            (next, consecutive)
        } else {
            self.failed_checks.fetch_add(1, Ordering::Relaxed);
            let consecutive = self
                .consecutive_failures
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            self.consecutive_successes.store(0, Ordering::Relaxed);
            self.last_failure.store(
                failure.map_or(0, |failure| failure as u8),
                Ordering::Relaxed,
            );
            let next = if matches!(
                previous,
                EndpointHealthState::Unknown | EndpointHealthState::Healthy
            ) && consecutive >= u64::from(unhealthy_threshold)
            {
                EndpointHealthState::Unhealthy
            } else {
                previous
            };
            (next, consecutive)
        };
        let transition = if next == previous {
            None
        } else {
            self.state.store(next as u8, Ordering::Release);
            if let Some(at_unix_ms) = at_unix_ms {
                self.last_transition_at_unix_ms
                    .store(at_unix_ms, Ordering::Relaxed);
            }
            Some((previous, next))
        };
        let passive_recovery = (healthy && passive_policy.readmit_by == ReadmitBy::Backoff)
            .then_some(consecutive)
            .filter(|consecutive| {
                *consecutive >= u64::from(healthy_threshold.max(passive_policy.recovery_threshold))
            })
            .and_then(|_| {
                self.recover_passive(at_unix_ms.unwrap_or_else(now_unix_ms), passive_policy)
            });
        HealthUpdate {
            transition,
            passive_recovery,
        }
    }

    pub(super) fn record_passive_failure(
        &self,
        failure: HealthFailure,
        at_unix_ms: u64,
        policy: PassiveFailurePolicy,
        may_eject: impl FnOnce() -> bool,
    ) -> Option<PassiveEjection> {
        if !policy.observes(failure) {
            return None;
        }
        let failure_count = increment_saturating(&self.passive_failure_count);
        let consecutive = increment_saturating(&self.passive_consecutive_failures);
        self.passive_last_failure
            .store(failure as u8, Ordering::Relaxed);
        if self.passively_ejected_at(at_unix_ms)
            || consecutive < u64::from(policy.consecutive_failure_threshold)
            || !may_eject()
        {
            return None;
        }

        Some(self.eject(failure, failure_count, at_unix_ms, policy))
    }

    /// Starts the next ejection step: one longer than the previous, counted and timed.
    pub(super) fn eject(
        &self,
        failure: HealthFailure,
        failure_count: u64,
        at_unix_ms: u64,
        policy: PassiveFailurePolicy,
    ) -> PassiveEjection {
        let ejection_count = increment_saturating(&self.passive_ejection_count);
        let backoff_step = increment_saturating(&self.passive_backoff_step);
        let duration = policy.ejection_duration_for(backoff_step, ejection_entropy(at_unix_ms));
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let ejection_until_unix_ms = at_unix_ms.saturating_add(duration_ms);
        self.passive_last_failure
            .store(failure as u8, Ordering::Relaxed);
        self.awaiting_probe
            .store(policy.readmit_by == ReadmitBy::Probe, Ordering::Release);
        self.passive_ejected.store(true, Ordering::Release);
        self.passive_consecutive_failures
            .store(0, Ordering::Relaxed);
        self.passive_ejected_at_unix_ms
            .store(at_unix_ms, Ordering::Relaxed);
        self.passive_ejection_until_unix_ms
            .store(ejection_until_unix_ms, Ordering::Release);
        if policy.marks_down() {
            let previous = EndpointHealthState::from_u8(
                self.state
                    .swap(EndpointHealthState::Unhealthy as u8, Ordering::AcqRel),
            );
            if previous != EndpointHealthState::Unhealthy {
                self.last_transition_at_unix_ms
                    .store(at_unix_ms, Ordering::Relaxed);
            }
        }
        PassiveEjection {
            reason: failure,
            failure_count,
            ejection_count,
            ejected_at_unix_ms: at_unix_ms,
            ejection_until_unix_ms,
        }
    }

    /// A live success ends the run of consecutive failures.
    pub(super) fn record_passive_success(&self) {
        if self.passive_consecutive_failures.load(Ordering::Relaxed) != 0 {
            self.passive_consecutive_failures
                .store(0, Ordering::Relaxed);
        }
    }

    pub(super) fn recover_passive(
        &self,
        at_unix_ms: u64,
        policy: PassiveFailurePolicy,
    ) -> Option<PassiveRecovery> {
        if !self.passive_ejected.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.awaiting_probe.store(false, Ordering::Release);
        let recovery_count = increment_saturating(&self.passive_recovery_count);
        self.passive_consecutive_failures
            .store(0, Ordering::Relaxed);
        self.passive_backoff_step.store(0, Ordering::Relaxed);
        self.passive_last_recovery_at_unix_ms
            .store(at_unix_ms, Ordering::Relaxed);
        if policy.mark_up {
            let previous = EndpointHealthState::from_u8(
                self.state
                    .swap(EndpointHealthState::Healthy as u8, Ordering::AcqRel),
            );
            if previous != EndpointHealthState::Healthy {
                self.last_transition_at_unix_ms
                    .store(at_unix_ms, Ordering::Relaxed);
            }
        }
        Some(PassiveRecovery {
            reason: HealthFailure::from_u8(self.passive_last_failure.load(Ordering::Relaxed)),
            recovery_count,
            recovered_at_unix_ms: at_unix_ms,
        })
    }

    pub(super) fn snapshot(&self, now_unix_ms: u64) -> EndpointHealthSnapshot {
        EndpointHealthSnapshot {
            active_connections: self.active_work.load(Ordering::Relaxed),
            administrative_state: self.administrative_state(),
            address: self.endpoint.clone(),
            checks_enabled: self.checks_enabled.load(Ordering::Acquire),
            checks_running: self.checks_running(),
            configured_max_connections: self.configured_max_connections(),
            health_override: self.health_override(),
            max_connections: self.configured_limit(),
            name: self.name.clone(),
            state: self.state(),
            origin: self.origin,
            configured_weight: self.configured_weight(),
            weight: self.weight(),
            last_checked_at_unix_ms: nonzero(self.last_checked_at_unix_ms.load(Ordering::Relaxed)),
            last_transition_at_unix_ms: nonzero(
                self.last_transition_at_unix_ms.load(Ordering::Relaxed),
            ),
            successful_checks: self.successful_checks.load(Ordering::Relaxed),
            failed_checks: self.failed_checks.load(Ordering::Relaxed),
            consecutive_successes: self.consecutive_successes.load(Ordering::Relaxed),
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            last_failure: HealthFailure::from_u8(self.last_failure.load(Ordering::Relaxed)),
            passive_ejected: self.passively_ejected_at(now_unix_ms),
            passive_failure_count: self.passive_failure_count.load(Ordering::Relaxed),
            passive_consecutive_failures: self.passive_consecutive_failures.load(Ordering::Relaxed),
            passive_ejection_count: self.passive_ejection_count.load(Ordering::Relaxed),
            passive_ejection_reason: HealthFailure::from_u8(
                self.passive_last_failure.load(Ordering::Relaxed),
            ),
            passive_ejected_at_unix_ms: nonzero(
                self.passive_ejected_at_unix_ms.load(Ordering::Relaxed),
            ),
            passive_ejection_until_unix_ms: nonzero(
                self.passive_ejection_until_unix_ms.load(Ordering::Relaxed),
            ),
            passive_recovery_count: self.passive_recovery_count.load(Ordering::Relaxed),
            passive_last_recovery_at_unix_ms: nonzero(
                self.passive_last_recovery_at_unix_ms
                    .load(Ordering::Relaxed),
            ),
        }
    }
}
