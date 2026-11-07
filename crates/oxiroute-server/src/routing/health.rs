#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum EndpointHealthState {
    Unchecked,
    Unknown,
    Healthy,
    Unhealthy,
}

impl EndpointHealthState {
    pub(super) const fn selectable(self) -> bool {
        matches!(self, Self::Unchecked | Self::Healthy)
    }

    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Unchecked,
            2 => Self::Healthy,
            3 => Self::Unhealthy,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum HealthFailure {
    Timeout = 1,
    ConnectFailed = 2,
    UnexpectedStatus = 3,
    ProtocolError = 4,
}

const DEFAULT_PASSIVE_FAILURE_THRESHOLD: u16 = 3;
const DEFAULT_PASSIVE_EJECTION_DURATION: Duration = Duration::from_secs(30);
const DEFAULT_PASSIVE_MAX_EJECTION_DURATION: Duration = Duration::from_mins(5);
const MAX_PASSIVE_FAILURE_THRESHOLD: u16 = 100;
const MAX_PASSIVE_EJECTION_DURATION: Duration = Duration::from_hours(24);
const MAX_EJECTION_JITTER_PERCENT: u8 = 50;
pub(super) const RECOVERY_POLL: Duration = Duration::from_millis(250);

/// Bounded policy for passively ejecting endpoints after attributed upstream failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PassiveFailurePolicy {
    enabled: bool,
    pub consecutive_failure_threshold: u16,
    pub initial_ejection_duration: Duration,
    pub max_ejection_duration: Duration,
    /// Each ejection lasts its backoff duration plus or minus up to this many percent.
    pub ejection_jitter_percent: u8,
    pub observe: PassiveObserve,
    pub on_error: PassiveOnError,
    pub mark_down: bool,
    pub mark_up: bool,
    pub recovery_threshold: u16,
    pub readmit_by: ReadmitBy,
    pub circuit_breaker: Option<CircuitBreakerPolicy>,
}

/// What returns a passively ejected server to rotation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReadmitBy {
    /// The ejection time ending.
    #[default]
    Backoff,
    /// An active probe succeeding; the time only starts the probing.
    Probe,
}

impl PassiveFailurePolicy {
    #[must_use]
    pub const fn new(
        consecutive_failure_threshold: u16,
        initial_ejection_duration: Duration,
        max_ejection_duration: Duration,
    ) -> Self {
        Self {
            enabled: true,
            consecutive_failure_threshold,
            initial_ejection_duration,
            max_ejection_duration,
            ejection_jitter_percent: 0,
            observe: PassiveObserve::Layer7,
            on_error: PassiveOnError::Count,
            mark_down: false,
            mark_up: false,
            recovery_threshold: 1,
            readmit_by: ReadmitBy::Backoff,
            circuit_breaker: None,
        }
    }

    #[must_use]
    pub(crate) fn from_config(policy: &ConfigPassiveHealthPolicy) -> Self {
        Self {
            enabled: true,
            consecutive_failure_threshold: match policy.on_error {
                PassiveOnError::Count | PassiveOnError::MarkDown => policy.error_limit,
                PassiveOnError::Immediately => 1,
            },
            initial_ejection_duration: Duration::from_millis(policy.initial_backoff_ms),
            max_ejection_duration: Duration::from_millis(policy.max_backoff_ms),
            ejection_jitter_percent: 0,
            observe: policy.observe,
            on_error: policy.on_error,
            mark_down: policy.mark_down,
            mark_up: policy.mark_up,
            recovery_threshold: policy.recovery_threshold,
            readmit_by: if policy.readmission.is_some() {
                ReadmitBy::Probe
            } else {
                ReadmitBy::Backoff
            },
            circuit_breaker: policy
                .circuit_breaker
                .as_ref()
                .map(CircuitBreakerPolicy::from),
        }
    }

    #[must_use]
    pub const fn with_ejection_jitter_percent(mut self, percent: u8) -> Self {
        self.ejection_jitter_percent = percent;
        self
    }

    pub(super) fn observes(self, failure: HealthFailure) -> bool {
        if !self.enabled {
            return false;
        }
        match self.observe {
            PassiveObserve::Layer4 => {
                matches!(
                    failure,
                    HealthFailure::ConnectFailed | HealthFailure::Timeout
                )
            }
            PassiveObserve::Layer7 => true,
        }
    }

    pub(super) fn marks_down(self) -> bool {
        self.mark_down || matches!(self.on_error, PassiveOnError::MarkDown)
    }

    pub(crate) fn validate(self) -> Result<(), PoolError> {
        if self.consecutive_failure_threshold == 0
            || self.consecutive_failure_threshold > MAX_PASSIVE_FAILURE_THRESHOLD
        {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "consecutive failure threshold must be between 1 and 100",
            });
        }
        if self.initial_ejection_duration.is_zero() {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "initial ejection duration must be nonzero",
            });
        }
        if self.initial_ejection_duration > self.max_ejection_duration {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "maximum ejection duration must not be shorter than the initial duration",
            });
        }
        if self.max_ejection_duration > MAX_PASSIVE_EJECTION_DURATION {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "maximum ejection duration must not exceed 24 hours",
            });
        }
        if self.ejection_jitter_percent > MAX_EJECTION_JITTER_PERCENT {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "ejection jitter must not exceed 50 percent",
            });
        }
        if self.recovery_threshold == 0 || self.recovery_threshold > MAX_PASSIVE_FAILURE_THRESHOLD {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "recovery threshold must be between 1 and 100",
            });
        }
        Ok(())
    }

    /// The backoff duration for `backoff_step`, spread by the jitter percentage using `entropy`
    /// and never longer than the maximum.
    pub(super) fn ejection_duration_for(self, backoff_step: u64, entropy: u64) -> Duration {
        let exponent = backoff_step.saturating_sub(1).min(31);
        let multiplier = 1_u32 << exponent;
        let backoff = self
            .initial_ejection_duration
            .checked_mul(multiplier)
            .map_or(self.max_ejection_duration, |duration| {
                duration.min(self.max_ejection_duration)
            });
        let backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
        let spread_ms = backoff_ms / 100 * u64::from(self.ejection_jitter_percent);
        let jittered_ms = backoff_ms - spread_ms + entropy % (2 * spread_ms + 1);
        Duration::from_millis(jittered_ms).min(self.max_ejection_duration)
    }
}

impl Default for PassiveFailurePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            consecutive_failure_threshold: DEFAULT_PASSIVE_FAILURE_THRESHOLD,
            initial_ejection_duration: DEFAULT_PASSIVE_EJECTION_DURATION,
            max_ejection_duration: DEFAULT_PASSIVE_MAX_EJECTION_DURATION,
            ejection_jitter_percent: 0,
            observe: PassiveObserve::Layer7,
            on_error: PassiveOnError::Count,
            mark_down: false,
            mark_up: false,
            recovery_threshold: 1,
            readmit_by: ReadmitBy::Backoff,
            circuit_breaker: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum AdministrativeState {
    #[default]
    Ready,
    Drain,
    Maintenance,
}

impl AdministrativeState {
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Drain,
            2 => Self::Maintenance,
            _ => Self::Ready,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum HealthOverride {
    #[default]
    Auto,
    Up,
    Down,
}

impl HealthOverride {
    pub(super) const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Up,
            2 => Self::Down,
            _ => Self::Auto,
        }
    }
}

/// Runtime administrative state of one server, keyed by pool and server name so it can move to the
/// next generation. Only servers that differ from the defaults are carried.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerAdminState {
    pub pool: String,
    pub server: String,
    #[serde(default)]
    pub administrative: AdministrativeState,
    #[serde(default)]
    pub health: HealthOverride,
    #[serde(default = "checks_enabled_by_default")]
    pub checks_enabled: bool,
    #[serde(default)]
    pub max_connections: Option<u64>,
    #[serde(default)]
    pub weight: Option<u16>,
}

const fn checks_enabled_by_default() -> bool {
    true
}

impl ServerAdminState {
    pub(super) fn is_default(&self) -> bool {
        self.administrative == AdministrativeState::Ready
            && self.health == HealthOverride::Auto
            && self.checks_enabled
            && self.max_connections.is_none()
            && self.weight.is_none()
    }
}

impl HealthFailure {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::ConnectFailed => "connect_failed",
            Self::UnexpectedStatus => "unexpected_status",
            Self::ProtocolError => "protocol_error",
        }
    }

    pub(super) const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Timeout),
            2 => Some(Self::ConnectFailed),
            3 => Some(Self::UnexpectedStatus),
            4 => Some(Self::ProtocolError),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct HealthUpdate {
    pub(super) transition: Option<(EndpointHealthState, EndpointHealthState)>,
    pub(super) passive_recovery: Option<PassiveRecovery>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PassiveEjection {
    pub(super) reason: HealthFailure,
    pub(super) failure_count: u64,
    pub(super) ejection_count: u64,
    pub(super) ejected_at_unix_ms: u64,
    pub(super) ejection_until_unix_ms: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PassiveRecovery {
    pub(super) reason: Option<HealthFailure>,
    pub(super) recovery_count: u64,
    pub(super) recovered_at_unix_ms: u64,
}

pub(super) const fn nonzero(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

pub(super) fn increment_saturating(counter: &AtomicU64) -> u64 {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        })
        .unwrap_or(u64::MAX)
        .saturating_add(1)
}

pub(super) fn ejection_entropy(salt: u64) -> u64 {
    RandomState::new().hash_one(salt)
}

pub(super) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointHealthSnapshot {
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub active_connections: u64,
    pub administrative_state: AdministrativeState,
    pub address: RuntimeEndpoint,
    pub checks_enabled: bool,
    pub checks_running: bool,
    pub configured_max_connections: Option<u64>,
    pub health_override: HealthOverride,
    pub max_connections: Option<u64>,
    pub name: String,
    pub state: EndpointHealthState,
    pub configured_weight: u16,
    pub weight: u16,
    pub last_checked_at_unix_ms: Option<u64>,
    pub last_transition_at_unix_ms: Option<u64>,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub successful_checks: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub failed_checks: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub consecutive_successes: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub consecutive_failures: u64,
    pub last_failure: Option<HealthFailure>,
    pub passive_ejected: bool,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_failure_count: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_consecutive_failures: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_ejection_count: u64,
    pub passive_ejection_reason: Option<HealthFailure>,
    pub passive_ejected_at_unix_ms: Option<u64>,
    pub passive_ejection_until_unix_ms: Option<u64>,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_recovery_count: u64,
    pub passive_last_recovery_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolHealthSnapshot {
    pub name: String,
    pub algorithm: &'static str,
    pub available_endpoints: usize,
    pub total_endpoints: usize,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub unavailable_selections: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub fail_open_selections: u64,
    pub queued: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub oldest_queue_wait_ms: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub queued_total: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub queue_timeouts: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub queue_cancellations: u64,
    pub endpoints: Vec<EndpointHealthSnapshot>,
}

#[derive(Debug)]
pub(super) struct PoolHealthState {
    endpoints: Arc<[Arc<PoolEndpoint>]>,
    pub(super) health_version: AtomicU64,
    pub(super) health_writer: Mutex<()>,
    passive_policy: PassiveFailurePolicy,
    pub(super) pool_name: String,
    queue: Arc<PoolQueue>,
    readmission: Option<Arc<ReadmissionProbe>>,
    resilience: PoolResiliencePolicy,
}

impl PoolHealthState {
    pub(super) fn new(
        pool_name: String,
        endpoints: Arc<[Arc<PoolEndpoint>]>,
        queue: Arc<PoolQueue>,
        passive_policy: PassiveFailurePolicy,
        resilience: PoolResiliencePolicy,
        readmission: Option<Arc<ReadmissionProbe>>,
    ) -> Self {
        Self {
            endpoints,
            health_version: AtomicU64::new(0),
            health_writer: Mutex::new(()),
            passive_policy,
            pool_name,
            queue,
            readmission,
            resilience,
        }
    }

    /// Whether ejecting `server` still leaves the configured floor of other selectable servers.
    fn may_eject(&self, server: &PoolEndpoint, at_unix_ms: u64) -> bool {
        self.endpoints
            .iter()
            .filter(|other| {
                !std::ptr::eq(other.as_ref(), server) && other.selectable_at(at_unix_ms)
            })
            .count()
            >= self.resilience.min_members
    }

    pub(super) fn record_passive_failure(
        self: &Arc<Self>,
        server: &Arc<PoolEndpoint>,
        failure: HealthFailure,
    ) {
        self.record_passive_failure_at(server, failure, now_unix_ms());
    }

    pub(super) fn record_passive_failure_at(
        self: &Arc<Self>,
        server: &Arc<PoolEndpoint>,
        failure: HealthFailure,
        at_unix_ms: u64,
    ) {
        let ejection = {
            let _writer = self
                .health_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.health_version.fetch_add(1, Ordering::AcqRel);
            let ejection = server.record_passive_failure(
                failure,
                at_unix_ms,
                self.passive_policy,
                self.may_eject(server, at_unix_ms),
            );
            self.health_version.fetch_add(1, Ordering::Release);
            ejection
        };
        if self.passive_policy.observes(failure) {
            self.record_outcome(server, false, at_unix_ms);
        }
        if let Some(ejection) = ejection {
            self.queue.notify_capacity_waiters();
            crate::operational_event::emit_upstream_endpoint_ejection(
                &self.pool_name,
                &server.name,
                ejection.reason,
                ejection.failure_count,
                ejection.ejection_count,
                ejection.ejected_at_unix_ms,
                ejection.ejection_until_unix_ms,
            );
            self.start_probing(server);
        }
    }

    pub(super) fn record_passive_success(&self, server: &PoolEndpoint) {
        server.record_passive_success();
        self.record_outcome(server, true, now_unix_ms());
    }

    /// Feeds the server's circuit breaker; opening respects `min_members` like an ejection.
    fn record_outcome(&self, server: &PoolEndpoint, ok: bool, at_unix_ms: u64) {
        let Some(policy) = self.passive_policy.circuit_breaker else {
            return;
        };
        let may_open = self.may_eject(server, at_unix_ms);
        match server.breaker.record(ok, at_unix_ms, policy, may_open) {
            Some(BreakerTransition::Opened) => {
                self.queue.notify_capacity_waiters();
                log::warn!(
                    "upstream pool `{}` server `{}` circuit breaker opened",
                    self.pool_name,
                    server.name
                );
            }
            Some(BreakerTransition::Closed) => {
                self.queue.notify_capacity_waiters();
                log::info!(
                    "upstream pool `{}` server `{}` circuit breaker closed",
                    self.pool_name,
                    server.name
                );
            }
            None => {}
        }
    }

    /// Probes an ejected server until it passes, so live requests never have to test it.
    fn start_probing(self: &Arc<Self>, server: &Arc<PoolEndpoint>) {
        let Some(probe) = &self.readmission else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if !server.probing.swap(true, Ordering::AcqRel) {
            runtime.spawn(probe_until_readmitted(
                Arc::downgrade(self),
                Arc::downgrade(server),
                Arc::clone(probe),
            ));
        }
    }

    /// A failed probe ends the quiet period with the next, longer ejection step.
    fn reject_probe(&self, server: &PoolEndpoint, failure: HealthFailure) {
        let at_unix_ms = now_unix_ms();
        let ejection = {
            let _writer = self
                .health_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.health_version.fetch_add(1, Ordering::AcqRel);
            let failure_count = server.passive_failure_count.load(Ordering::Relaxed);
            let ejection = server.eject(failure, failure_count, at_unix_ms, self.passive_policy);
            self.health_version.fetch_add(1, Ordering::Release);
            ejection
        };
        crate::operational_event::emit_upstream_endpoint_ejection(
            &self.pool_name,
            &server.name,
            ejection.reason,
            ejection.failure_count,
            ejection.ejection_count,
            ejection.ejected_at_unix_ms,
            ejection.ejection_until_unix_ms,
        );
    }

    fn readmit(&self, server: &PoolEndpoint) {
        let recovery = {
            let _writer = self
                .health_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.health_version.fetch_add(1, Ordering::AcqRel);
            let recovery = server.recover_passive(now_unix_ms(), self.passive_policy);
            self.health_version.fetch_add(1, Ordering::Release);
            recovery
        };
        if let Some(recovery) = recovery {
            self.queue.notify_capacity_waiters();
            crate::operational_event::emit_upstream_endpoint_recovery(
                &self.pool_name,
                &server.name,
                recovery.reason,
                recovery.recovery_count,
                recovery.recovered_at_unix_ms,
            );
        }
    }

    pub(super) fn record_health(
        &self,
        server: &PoolEndpoint,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
    ) -> HealthUpdate {
        let update = {
            let _writer = self
                .health_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.health_version.fetch_add(1, Ordering::AcqRel);
            let update = server.record(
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
                self.passive_policy,
            );
            self.health_version.fetch_add(1, Ordering::Release);
            update
        };
        if update.transition.is_some() || update.passive_recovery.is_some() {
            self.queue.notify_capacity_waiters();
        }
        if let Some(recovery) = update.passive_recovery {
            crate::operational_event::emit_upstream_endpoint_recovery(
                &self.pool_name,
                &server.name,
                recovery.reason,
                recovery.recovery_count,
                recovery.recovered_at_unix_ms,
            );
        }
        update
    }
}

/// Waits out the ejection, then probes until `successes` in a row pass; a failed probe re-ejects.
async fn probe_until_readmitted(
    health: std::sync::Weak<PoolHealthState>,
    server: std::sync::Weak<PoolEndpoint>,
    probe: Arc<ReadmissionProbe>,
) {
    let mut streak = 0_u16;
    loop {
        let wait = {
            let Some(server) = server.upgrade() else {
                return;
            };
            if !server.passive_ejected.load(Ordering::Acquire) {
                server.probing.store(false, Ordering::Release);
                return;
            }
            let quiet = server
                .passive_ejection_until_unix_ms
                .load(Ordering::Acquire)
                .saturating_sub(now_unix_ms());
            if quiet > 0 {
                Duration::from_millis(quiet)
            } else {
                probe.interval
            }
        };
        tokio::time::sleep(wait).await;
        let (Some(health), Some(server)) = (health.upgrade(), server.upgrade()) else {
            return;
        };
        if !server.passive_ejected.load(Ordering::Acquire) {
            server.probing.store(false, Ordering::Release);
            return;
        }
        let result = match server.resolve_addresses().await {
            Ok(addresses) => probe.probe(&addresses).await,
            Err(_) => Err(HealthFailure::ConnectFailed),
        };
        match result {
            Ok(()) => {
                streak += 1;
                if streak >= probe.successes {
                    health.readmit(&server);
                    server.probing.store(false, Ordering::Release);
                    return;
                }
            }
            Err(failure) => {
                streak = 0;
                health.reject_probe(&server, failure);
            }
        }
    }
}
