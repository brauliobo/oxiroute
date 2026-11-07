#[allow(clippy::wildcard_imports)]
use super::*;

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

#[derive(Debug)]
pub(super) struct PoolEndpoint {
    pub(super) active_work: AtomicU64,
    pub(super) administrative_state: AtomicU8,
    pub(super) checks_enabled: AtomicBool,
    pub(super) health_override: AtomicU8,
    pub(super) endpoint: RuntimeEndpoint,
    configured_max_connections: Option<u64>,
    pub(super) max_connections_override: AtomicU64,
    pub(super) name: String,
    pinned_addresses: Option<RwLock<Arc<[SocketAddr]>>>,
    protected_addresses: Arc<[SocketAddr]>,
    pub(super) state: AtomicU8,
    weight: u16,
    pub(super) last_checked_at_unix_ms: AtomicU64,
    pub(super) last_transition_at_unix_ms: AtomicU64,
    pub(super) successful_checks: AtomicU64,
    pub(super) failed_checks: AtomicU64,
    pub(super) consecutive_successes: AtomicU64,
    pub(super) consecutive_failures: AtomicU64,
    last_failure: AtomicU8,
    passive_ejected: AtomicBool,
    passive_failure_count: AtomicU64,
    passive_consecutive_failures: AtomicU64,
    passive_ejection_count: AtomicU64,
    passive_backoff_step: AtomicU64,
    passive_last_failure: AtomicU8,
    passive_ejected_at_unix_ms: AtomicU64,
    passive_ejection_until_unix_ms: AtomicU64,
    passive_last_recovery_at_unix_ms: AtomicU64,
    passive_recovery_count: AtomicU64,
}

impl PoolEndpoint {
    pub(super) fn new(server: RuntimeServer, startup: Option<HealthStartup>, weight: u16) -> Self {
        let state = match startup {
            None => EndpointHealthState::Unchecked,
            Some(HealthStartup::Healthy) => EndpointHealthState::Healthy,
            Some(HealthStartup::Unhealthy) => EndpointHealthState::Unhealthy,
            Some(HealthStartup::Checking) => EndpointHealthState::Unknown,
        };
        Self {
            active_work: AtomicU64::new(0),
            administrative_state: AtomicU8::new(AdministrativeState::Ready as u8),
            checks_enabled: AtomicBool::new(true),
            health_override: AtomicU8::new(HealthOverride::Auto as u8),
            endpoint: server.endpoint,
            configured_max_connections: server.max_connections,
            max_connections_override: AtomicU64::new(0),
            name: server.name,
            pinned_addresses: server.pinned_addresses.map(RwLock::new),
            protected_addresses: server.protected_addresses,
            state: AtomicU8::new(state as u8),
            weight,
            last_checked_at_unix_ms: AtomicU64::new(0),
            last_transition_at_unix_ms: AtomicU64::new(0),
            successful_checks: AtomicU64::new(0),
            failed_checks: AtomicU64::new(0),
            consecutive_successes: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            last_failure: AtomicU8::new(0),
            passive_ejected: AtomicBool::new(false),
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
            && self.health_permits()
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
            && self.passive_ejection_until_unix_ms.load(Ordering::Acquire) > now_unix_ms
    }

    pub(super) fn has_capacity(&self) -> bool {
        self.max_connections()
            .is_none_or(|limit| self.active_work.load(Ordering::Acquire) < limit)
    }

    pub(super) fn administrative_state(&self) -> AdministrativeState {
        AdministrativeState::from_u8(self.administrative_state.load(Ordering::Acquire))
    }

    pub(super) fn health_override(&self) -> HealthOverride {
        HealthOverride::from_u8(self.health_override.load(Ordering::Acquire))
    }

    fn max_connections(&self) -> Option<u64> {
        match self.max_connections_override.load(Ordering::Acquire) {
            0 => self.configured_max_connections,
            limit => Some(limit),
        }
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
        if !admission.admits(self, now_unix_ms) {
            return None;
        }
        self.try_acquire_capacity(admission, now_unix_ms)?;
        if !admission.admits(self, now_unix_ms) {
            self.release_capacity(queue);
            return None;
        }
        Some(EndpointLease::acquired(
            Arc::clone(self),
            Arc::clone(queue),
            Arc::clone(health),
        ))
    }

    pub(super) fn try_acquire_capacity(
        &self,
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
        Some(())
    }

    pub(super) fn release_capacity(&self, queue: &PoolQueue) {
        let released = self
            .active_work
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            });
        debug_assert!(released.is_ok(), "endpoint lease counter underflow");
        queue.notify_capacity_waiters();
    }

    pub(super) async fn resolve_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        let addresses = if let Some(addresses) = &self.pinned_addresses {
            addresses
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .to_vec()
        } else {
            self.endpoint.resolve_addresses().await?
        };
        self.reject_protected_addresses(&addresses)?;
        Ok(addresses)
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
        let passive_recovery = healthy
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
        may_eject: bool,
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
            || !may_eject
        {
            return None;
        }

        let ejection_count = increment_saturating(&self.passive_ejection_count);
        let backoff_step = increment_saturating(&self.passive_backoff_step);
        let duration = policy.ejection_duration_for(backoff_step, ejection_entropy(at_unix_ms));
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let ejection_until_unix_ms = at_unix_ms.saturating_add(duration_ms);
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
        Some(PassiveEjection {
            reason: failure,
            failure_count,
            ejection_count,
            ejected_at_unix_ms: at_unix_ms,
            ejection_until_unix_ms,
        })
    }

    fn recover_passive(
        &self,
        at_unix_ms: u64,
        policy: PassiveFailurePolicy,
    ) -> Option<PassiveRecovery> {
        if !self.passive_ejected.swap(false, Ordering::AcqRel) {
            return None;
        }
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
            configured_max_connections: self.configured_max_connections,
            health_override: self.health_override(),
            max_connections: self.max_connections(),
            name: self.name.clone(),
            state: self.state(),
            weight: self.weight,
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
