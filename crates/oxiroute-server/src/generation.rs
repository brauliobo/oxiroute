use std::{
    collections::HashMap,
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_config::Config;
use oxiroute_config_source::ConfigFormat;
use oxiroute_rtmp::{
    RtmpRecorderLifecycle, RtmpRecorderShutdown, RtmpRegistry, RtmpServiceRuntime,
};
use pingora::apps::{AcceptGate, AcceptGateClose, AcceptOwnership};
use serde::Serialize;

use crate::{
    ListenerReservations, ProcessRuntime, RuntimeMetrics, RuntimePlan, ServiceKind,
    config_coordinator::{CanonicalConfigDocument, ConfigRevision},
    runtime_plan,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeReferenceKind {
    ForwardHttp1,
    Http1,
    Http2,
    WebSocket,
    Tcp,
    Rtmp,
}

impl RuntimeReferenceKind {
    const fn index(self) -> usize {
        match self {
            Self::ForwardHttp1 => 0,
            Self::Http1 => 1,
            Self::Http2 => 2,
            Self::WebSocket => 3,
            Self::Tcp => 4,
            Self::Rtmp => 5,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRevision {
    pub disk: ConfigRevision,
    pub candidate: ConfigRevision,
}

pub struct PreparedGeneration {
    config: Arc<Config>,
    metrics: RuntimeMetrics,
    plan: RuntimePlan,
    reservations: ListenerReservations,
    revision: GenerationRevision,
    rtmp_registry: Arc<RtmpRegistry>,
    rtmp_runtimes: HashMap<String, RtmpServiceRuntime>,
}

impl PreparedGeneration {
    fn prepare(
        document: CanonicalConfigDocument,
        previous: Option<&ListenerReservations>,
        process: ProcessRuntime,
    ) -> Result<Self, GenerationError> {
        let config = Arc::new(document.normalized_config);
        let plan =
            runtime_plan(&config).map_err(|source| GenerationError::Plan(Box::new(source)))?;
        let reservations = ListenerReservations::prepare(&config, previous)?;
        crate::stats::preflight_admin_token(
            config
                .stats
                .as_ref()
                .and_then(|stats| stats.admin_token_file.as_deref()),
        )?;
        if config.management.is_some() {
            let token_file = std::env::var_os("OXIROUTE_MANAGEMENT_TOKEN_FILE")
                .map(std::path::PathBuf::from)
                .ok_or(GenerationError::ManagementToken)?;
            crate::rtmp_api::preflight_management_token(&token_file)
                .map_err(|_| GenerationError::ManagementToken)?;
        }
        if let Some(ui_dir) = config
            .management
            .as_ref()
            .and_then(|management| management.ui_dir.as_deref())
        {
            crate::rtmp_api::UiAssets::load(ui_dir).map_err(|_| GenerationError::RuntimePrepare)?;
        }
        plan.tls
            .check_certbot_watcher(crate::CertbotWatcherConfig::default())
            .map_err(|_| GenerationError::RuntimePrepare)?;
        let metrics = RuntimeMetrics::for_process(process);
        metrics.register_upstream_pools(plan.pools.iter().cloned())?;
        let rtmp_registry = Arc::new(RtmpRegistry::new(plan.rtmp_capabilities));
        let mut rtmp_runtimes = HashMap::new();
        for service in &plan.services {
            let ServiceKind::Rtmp(service) = &service.kind else {
                continue;
            };
            if !rtmp_runtimes.contains_key(service.service_id()) {
                let runtime = service.runtime(Arc::clone(&rtmp_registry))?;
                rtmp_runtimes.insert(service.service_id().to_owned(), runtime);
            }
        }
        metrics.set_rtmp_recording_supported(plan.rtmp_recording_supported);
        Ok(Self {
            config,
            metrics,
            plan,
            reservations,
            revision: GenerationRevision {
                disk: document.disk_revision,
                candidate: document.candidate_revision,
            },
            rtmp_registry,
            rtmp_runtimes,
        })
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        &self.revision
    }
}

pub struct RuntimeGeneration {
    accept_gate: AcceptGate,
    config: Arc<Config>,
    drain: (Mutex<()>, Condvar),
    metrics: RuntimeMetrics,
    plan: RuntimePlan,
    references: [AtomicU64; 6],
    mutations: AtomicU64,
    reservations: ListenerReservations,
    revision: GenerationRevision,
    rtmp_registry: Arc<RtmpRegistry>,
    rtmp_runtimes: HashMap<String, RtmpServiceRuntime>,
    runtime_lifecycle: AtomicU8,
    runtime_failed: AtomicBool,
}

const RUNTIME_PREPARED: u8 = 0;
const RUNTIME_START_CLAIMED: u8 = 1;
const RUNTIME_STARTED: u8 = 2;
const RUNTIME_CANCELLED: u8 = 3;

impl RuntimeGeneration {
    fn activate(prepared: PreparedGeneration) -> Self {
        Self {
            accept_gate: AcceptGate::closed(),
            config: prepared.config,
            drain: (Mutex::new(()), Condvar::new()),
            metrics: prepared.metrics,
            plan: prepared.plan,
            references: std::array::from_fn(|_| AtomicU64::new(0)),
            mutations: AtomicU64::new(0),
            reservations: prepared.reservations,
            revision: prepared.revision,
            rtmp_registry: prepared.rtmp_registry,
            rtmp_runtimes: prepared.rtmp_runtimes,
            runtime_lifecycle: AtomicU8::new(RUNTIME_PREPARED),
            runtime_failed: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub fn config(&self) -> &Arc<Config> {
        &self.config
    }

    #[must_use]
    pub const fn plan(&self) -> &RuntimePlan {
        &self.plan
    }

    #[must_use]
    pub const fn metrics(&self) -> &RuntimeMetrics {
        &self.metrics
    }

    #[must_use]
    pub const fn reservations(&self) -> &ListenerReservations {
        &self.reservations
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        &self.revision
    }

    #[must_use]
    pub fn registry(&self) -> &Arc<RtmpRegistry> {
        &self.rtmp_registry
    }

    #[must_use]
    pub fn rtmp_runtime(&self, service: &str) -> Option<&RtmpServiceRuntime> {
        self.rtmp_runtimes.get(service)
    }

    fn close_runtime_admission(&self) {
        for runtime in self.rtmp_runtimes.values() {
            runtime.close_admission();
        }
    }

    pub fn initiate_recorder_shutdown(&self, deadline: Instant) -> Vec<RtmpRecorderShutdown> {
        self.close_runtime_admission();
        self.rtmp_runtimes
            .values()
            .filter_map(|runtime| runtime.initiate_recorder_shutdown(deadline))
            .collect()
    }

    fn recorder_lifecycles(&self) -> Vec<RtmpRecorderLifecycle> {
        self.rtmp_runtimes
            .values()
            .filter_map(RtmpServiceRuntime::recorder_lifecycle)
            .collect()
    }

    fn claim_runtime_start(&self) -> bool {
        self.runtime_lifecycle
            .compare_exchange(
                RUNTIME_PREPARED,
                RUNTIME_START_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[must_use]
    pub fn mark_runtime_started(&self) -> bool {
        self.runtime_lifecycle
            .compare_exchange(
                RUNTIME_START_CLAIMED,
                RUNTIME_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[must_use]
    pub fn runtime_started(&self) -> bool {
        self.runtime_lifecycle.load(Ordering::Acquire) == RUNTIME_STARTED
    }

    pub fn mark_runtime_failed(&self) {
        self.runtime_failed.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn runtime_failed(&self) -> bool {
        self.runtime_failed.load(Ordering::Acquire)
    }

    fn cancel_runtime_start(&self) {
        self.runtime_lifecycle
            .store(RUNTIME_CANCELLED, Ordering::Release);
        self.runtime_failed.store(true, Ordering::Release);
    }

    pub fn begin_reference(
        self: &Arc<Self>,
        kind: RuntimeReferenceKind,
    ) -> Option<GenerationReference> {
        if !self.accept_gate.accepting() {
            return None;
        }
        let counter = &self.references[kind.index()];
        counter.fetch_add(1, Ordering::AcqRel);
        if !self.accept_gate.accepting() {
            counter.fetch_sub(1, Ordering::AcqRel);
            self.drain.1.notify_all();
            return None;
        }
        Some(GenerationReference {
            generation: Arc::clone(self),
            kind,
        })
    }

    #[must_use]
    pub fn begin_owned_reference(
        self: &Arc<Self>,
        kind: RuntimeReferenceKind,
    ) -> GenerationReference {
        self.references[kind.index()].fetch_add(1, Ordering::AcqRel);
        GenerationReference {
            generation: Arc::clone(self),
            kind,
        }
    }

    pub fn begin_admission(self: &Arc<Self>) -> Option<GenerationAdmission> {
        self.accept_gate
            .claim()
            .map(|ownership| GenerationAdmission {
                generation: Arc::clone(self),
                _ownership: ownership,
            })
    }

    #[must_use]
    pub fn accept_gate(&self) -> AcceptGate {
        self.accept_gate.clone()
    }

    pub fn stop_accepting(&self) {
        let _ = self.accept_gate.close_and_wait(Duration::ZERO);
    }

    fn start_accepting(&self) {
        self.metrics.activate_limits(self.plan.max_connections);
        self.accept_gate.enable();
    }

    #[must_use]
    pub fn accepting(&self) -> bool {
        self.accept_gate.accepting()
    }

    #[must_use]
    pub fn active_references(&self, kind: RuntimeReferenceKind) -> u64 {
        self.references[kind.index()].load(Ordering::Acquire)
    }

    #[must_use]
    pub fn drained(&self) -> bool {
        self.references
            .iter()
            .all(|counter| counter.load(Ordering::Acquire) == 0)
    }

    #[must_use]
    pub fn drain(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        if !self.accept_gate.close_and_wait(timeout) {
            return false;
        }
        let timeout = timeout.saturating_sub(started.elapsed());
        let deadline = Instant::now() + timeout;
        let mut lock = self
            .drain
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if self.drained() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, wait) = self
                .drain
                .1
                .wait_timeout(lock, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lock = next;
            if wait.timed_out() {
                return self.drained();
            }
        }
    }
}

pub struct GenerationAdmission {
    generation: Arc<RuntimeGeneration>,
    _ownership: AcceptOwnership,
}

impl GenerationAdmission {
    #[must_use]
    pub fn accepting(&self) -> bool {
        self.generation.accepting()
    }
}

pub struct GenerationReference {
    generation: Arc<RuntimeGeneration>,
    kind: RuntimeReferenceKind,
}

pub struct GenerationMutation {
    generation: Arc<RuntimeGeneration>,
}

impl GenerationMutation {
    #[must_use]
    pub fn generation(&self) -> &Arc<RuntimeGeneration> {
        &self.generation
    }
}

impl Drop for GenerationMutation {
    fn drop(&mut self) {
        self.generation.mutations.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub struct GenerationCandidate {
    generation: Arc<RuntimeGeneration>,
    id: u64,
}

impl GenerationCandidate {
    #[must_use]
    pub fn generation(&self) -> &Arc<RuntimeGeneration> {
        &self.generation
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        self.generation.revision()
    }
}

impl Drop for GenerationReference {
    fn drop(&mut self) {
        self.generation.references[self.kind.index()].fetch_sub(1, Ordering::AcqRel);
        self.generation.drain.1.notify_all();
    }
}

#[derive(Default)]
struct GenerationState {
    active: Option<Arc<RuntimeGeneration>>,
    candidate: Option<GenerationCandidate>,
    disk_revision: Option<ConfigRevision>,
    last_failure: Option<&'static str>,
    previous: Option<Arc<RuntimeGeneration>>,
    quarantined_revision: Option<ConfigRevision>,
    shutdown_generations: Vec<Arc<RuntimeGeneration>>,
    shutting_down: bool,
    starting_candidate: Option<CandidateStartReservation>,
}

#[derive(Default)]
struct GenerationCleanupRegistry {
    recorder_lifecycles: Vec<RtmpRecorderLifecycle>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateStartPhase {
    Starting,
    RuntimeOwned,
    Activating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CandidateStartReservation {
    candidate_id: u64,
    phase: CandidateStartPhase,
    token: u64,
}

struct ActivationReservation {
    previous: Option<Arc<RuntimeGeneration>>,
    previous_revision: Option<ConfigRevision>,
    runtime_owned: bool,
    token: u64,
}

#[derive(Default)]
struct GenerationCounters {
    activations: AtomicU64,
    failures: AtomicU64,
    prepares: AtomicU64,
    rollbacks: AtomicU64,
}

#[derive(Clone)]
pub struct GenerationManager {
    #[cfg(test)]
    activation_hook: Arc<Mutex<Option<ActivationHook>>>,
    counters: Arc<GenerationCounters>,
    cleanup: Arc<Mutex<GenerationCleanupRegistry>>,
    next_candidate_id: Arc<AtomicU64>,
    next_reservation_token: Arc<AtomicU64>,
    operations: Arc<Mutex<()>>,
    process: ProcessRuntime,
    state: Arc<Mutex<GenerationState>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationHookPoint {
    Reserved,
    GateClosed,
}

#[cfg(test)]
type ActivationHook = Arc<dyn Fn(ActivationHookPoint) + Send + Sync>;

pub struct GenerationStartup {
    candidate: GenerationCandidate,
    manager: GenerationManager,
    reservation_token: u64,
    completed: bool,
}

impl GenerationStartup {
    /// Claims the single permitted runtime start for this candidate.
    ///
    /// # Errors
    ///
    /// Returns an error if the startup reservation or candidate is no longer current.
    pub fn claim_runtime_start(&mut self) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        self.manager
            .claim_runtime_start(&self.candidate, self.reservation_token)
    }

    pub fn activate(self) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        self.activate_with_timeout(Duration::from_secs(5))
    }

    fn activate_with_timeout(
        mut self,
        quiesce_timeout: Duration,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let activated = self.manager.activate_inner(
            &self.candidate,
            Some(self.reservation_token),
            quiesce_timeout,
        );
        self.completed = true;
        activated
    }
}

impl Drop for GenerationStartup {
    fn drop(&mut self) {
        if !self.completed {
            self.manager
                .cancel_startup(&self.candidate, self.reservation_token);
        }
    }
}

impl Default for GenerationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GenerationManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            activation_hook: Arc::new(Mutex::new(None)),
            counters: Arc::new(GenerationCounters::default()),
            cleanup: Arc::new(Mutex::new(GenerationCleanupRegistry::default())),
            next_candidate_id: Arc::new(AtomicU64::new(0)),
            next_reservation_token: Arc::new(AtomicU64::new(0)),
            operations: Arc::new(Mutex::new(())),
            process: ProcessRuntime::new(None),
            state: Arc::new(Mutex::new(GenerationState::default())),
        }
    }

    /// Fully prepares a candidate without writing the canonical configuration or publishing it.
    ///
    /// # Errors
    ///
    /// Returns a redacted preparation error. A failed candidate is released and active state is
    /// unchanged.
    pub fn prepare(
        &self,
        document: CanonicalConfigDocument,
    ) -> Result<GenerationCandidate, GenerationError> {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutting_down
        {
            return Err(GenerationError::MutationInProgress);
        }
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            .map(|generation| generation.reservations.clone());
        let disk_revision = document.disk_revision.clone();
        let candidate_revision = document.candidate_revision.clone();
        let prepared =
            PreparedGeneration::prepare(document, previous.as_ref(), self.process.clone())
                .map(|prepared| Arc::new(RuntimeGeneration::activate(prepared)));
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = Some(disk_revision.clone());
        match prepared {
            Ok(generation) => {
                let candidate = GenerationCandidate {
                    generation,
                    id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
                };
                crate::operational_event::emit(
                    "generation_prepare",
                    "prepared",
                    Some(&candidate.revision().candidate),
                );
                self.counters.prepares.fetch_add(1, Ordering::Relaxed);
                state.candidate = Some(candidate.clone());
                state.quarantined_revision = None;
                state.last_failure = None;
                Ok(candidate)
            }
            Err(error) => {
                crate::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.quarantined_revision = Some(candidate_revision);
                state.last_failure = Some(error.code());
                Err(error)
            }
        }
    }

    /// Performs the complete preparation path and releases the candidate without publishing it.
    ///
    /// # Errors
    ///
    /// Returns the same redacted preparation errors as [`Self::prepare`]. Active and pending state
    /// are unchanged.
    pub fn validate_candidate(
        &self,
        document: CanonicalConfigDocument,
    ) -> Result<(), GenerationError> {
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            .map(|generation| generation.reservations.clone());
        PreparedGeneration::prepare(document, previous.as_ref(), ProcessRuntime::new(None))
            .map(drop)
    }

    /// Atomically publishes the prepared candidate and stops new references to the old generation.
    ///
    /// # Errors
    ///
    /// Returns an error when no complete candidate is prepared.
    #[cfg(test)]
    pub(crate) fn activate(
        &self,
        candidate: &GenerationCandidate,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        self.activate_inner(candidate, None, Duration::from_secs(5))
    }

    fn activate_inner(
        &self,
        candidate: &GenerationCandidate,
        startup_token: Option<u64>,
        quiesce_timeout: Duration,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let reservation = self.reserve_activation(candidate, startup_token)?;
        #[cfg(test)]
        self.run_activation_hook(ActivationHookPoint::Reserved);
        let previous_close = reservation
            .previous
            .as_ref()
            .map(|previous| previous.accept_gate.close());
        #[cfg(test)]
        self.run_activation_hook(ActivationHookPoint::GateClosed);
        let quiesced = previous_close
            .as_ref()
            .is_none_or(|close| close.wait(quiesce_timeout));
        self.publish_reserved_candidate(candidate, &reservation, previous_close, quiesced)
    }

    fn reserve_activation(
        &self,
        candidate: &GenerationCandidate,
        startup_token: Option<u64>,
    ) -> Result<ActivationReservation, GenerationError> {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            if let Some(token) = startup_token {
                self.cancel_startup_locked(&mut state, candidate, token);
            }
            return Err(GenerationError::MutationInProgress);
        }
        let (token, runtime_owned) = if let Some(token) = startup_token {
            let Some(starting) = state.starting_candidate else {
                return Err(GenerationError::CandidateSuperseded);
            };
            if starting.candidate_id != candidate.id || starting.token != token {
                return Err(GenerationError::CandidateSuperseded);
            }
            if starting.phase == CandidateStartPhase::Starting {
                state.starting_candidate = None;
                return Err(GenerationError::RuntimePrepare);
            }
            if starting.phase != CandidateStartPhase::RuntimeOwned {
                return Err(GenerationError::CandidateSuperseded);
            }
            if !candidate_matches(state.candidate.as_ref(), candidate) {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
                return Err(GenerationError::CandidateSuperseded);
            }
            (token, true)
        } else {
            if !candidate_matches(state.candidate.as_ref(), candidate) {
                return Err(GenerationError::CandidateSuperseded);
            }
            if state.starting_candidate.is_some() {
                return Err(GenerationError::MutationInProgress);
            }
            (self.next_reservation_token(), false)
        };
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.mutations.load(Ordering::Acquire) != 0)
        {
            if runtime_owned {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
                self.quarantine_locked(&mut state, candidate, "runtime_start");
            }
            return Err(GenerationError::MutationInProgress);
        }
        if candidate.generation.runtime_failed()
            || (runtime_owned && !candidate.generation.runtime_started())
        {
            if runtime_owned {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
            }
            self.quarantine_locked(&mut state, candidate, "runtime_start");
            return Err(GenerationError::RuntimePrepare);
        }
        state.starting_candidate = Some(CandidateStartReservation {
            candidate_id: candidate.id,
            phase: CandidateStartPhase::Activating,
            token,
        });
        let previous = state.active.clone();
        Ok(ActivationReservation {
            previous_revision: previous
                .as_ref()
                .map(|generation| generation.revision.candidate.clone()),
            previous,
            runtime_owned,
            token,
        })
    }

    fn publish_reserved_candidate(
        &self,
        candidate: &GenerationCandidate,
        reservation: &ActivationReservation,
        previous_close: Option<AcceptGateClose>,
        quiesced: bool,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owns_reservation = state.starting_candidate
            == Some(CandidateStartReservation {
                candidate_id: candidate.id,
                phase: CandidateStartPhase::Activating,
                token: reservation.token,
            });
        let candidate_current = candidate_matches(state.candidate.as_ref(), candidate);
        let previous_current = active_matches_reservation(&state, reservation);
        let close_current = previous_close
            .as_ref()
            .is_none_or(AcceptGateClose::is_current);
        let failure = if state.shutting_down {
            Some(GenerationError::MutationInProgress)
        } else if !owns_reservation || !candidate_current || !previous_current {
            Some(GenerationError::CandidateSuperseded)
        } else if candidate.generation.runtime_failed() {
            Some(GenerationError::RuntimePrepare)
        } else if !quiesced || !close_current {
            Some(GenerationError::AcceptorQuiesce)
        } else if state
            .active
            .as_ref()
            .is_some_and(|active| active.mutations.load(Ordering::Acquire) != 0)
        {
            Some(GenerationError::MutationInProgress)
        } else {
            None
        };
        if let Some(error) = failure {
            if previous_current && !state.shutting_down {
                if let Some(close) = previous_close {
                    let _ = close.reopen();
                }
            }
            if owns_reservation {
                state.starting_candidate = None;
                if reservation.runtime_owned {
                    candidate.generation.cancel_runtime_start();
                    if candidate_current {
                        let reason = if matches!(error, GenerationError::RuntimePrepare) {
                            "runtime_start"
                        } else {
                            error.code()
                        };
                        self.quarantine_locked(&mut state, candidate, reason);
                    }
                }
            }
            return Err(error);
        }

        state.candidate = None;
        state.starting_candidate = None;
        let active = Arc::clone(&candidate.generation);
        if let Some(previous) = state.active.replace(Arc::clone(&active)) {
            let evicted = state.previous.replace(previous);
            if let Some(evicted) = &evicted {
                let mut cleanup = self
                    .cleanup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                cleanup
                    .recorder_lifecycles
                    .retain(|lifecycle| !lifecycle.is_complete());
                for lifecycle in evicted.recorder_lifecycles() {
                    push_unique_lifecycle(&mut cleanup.recorder_lifecycles, lifecycle);
                }
            }
        }
        active.start_accepting();
        state.last_failure = None;
        self.counters.activations.fetch_add(1, Ordering::Relaxed);
        crate::operational_event::emit(
            "generation_activate",
            "activated",
            Some(&active.revision.candidate),
        );
        drop(state);
        drop(operation);
        Ok(active)
    }

    fn next_reservation_token(&self) -> u64 {
        self.next_reservation_token
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    #[cfg(test)]
    fn set_activation_hook(&self, hook: ActivationHook) {
        *self
            .activation_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    fn run_activation_hook(&self, point: ActivationHookPoint) {
        let hook = self
            .activation_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }

    fn claim_runtime_start(
        &self,
        candidate: &GenerationCandidate,
        reservation_token: u64,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            return Err(GenerationError::MutationInProgress);
        }
        let expected = CandidateStartReservation {
            candidate_id: candidate.id,
            phase: CandidateStartPhase::Starting,
            token: reservation_token,
        };
        if state.starting_candidate != Some(expected)
            || !candidate_matches(state.candidate.as_ref(), candidate)
        {
            clear_starting_reservation(
                &mut state,
                candidate.id,
                reservation_token,
                CandidateStartPhase::Starting,
            );
            return Err(GenerationError::CandidateSuperseded);
        }
        if !candidate.generation.claim_runtime_start() {
            state.starting_candidate = None;
            self.quarantine_locked(&mut state, candidate, "runtime_start");
            return Err(GenerationError::RuntimePrepare);
        }
        state.starting_candidate = Some(CandidateStartReservation {
            candidate_id: candidate.id,
            phase: CandidateStartPhase::RuntimeOwned,
            token: reservation_token,
        });
        Ok(Arc::clone(&candidate.generation))
    }

    /// Reserves publication before any candidate runtime is started.
    ///
    /// # Errors
    ///
    /// Returns an error when the candidate was superseded, another startup is reserved, or an
    /// active-generation mutation currently prevents publication.
    pub fn begin_candidate_start(
        &self,
        candidate: &GenerationCandidate,
    ) -> Result<GenerationStartup, GenerationError> {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            return Err(GenerationError::MutationInProgress);
        }
        let pending = state
            .candidate
            .as_ref()
            .filter(|pending| {
                pending.id == candidate.id
                    && Arc::ptr_eq(&pending.generation, &candidate.generation)
            })
            .ok_or(GenerationError::CandidateSuperseded)?;
        if state.starting_candidate.is_some()
            || state
                .active
                .as_ref()
                .is_some_and(|active| active.mutations.load(Ordering::Acquire) != 0)
        {
            return Err(GenerationError::MutationInProgress);
        }
        let reservation_token = self.next_reservation_token();
        state.starting_candidate = Some(CandidateStartReservation {
            candidate_id: pending.id,
            phase: CandidateStartPhase::Starting,
            token: reservation_token,
        });
        Ok(GenerationStartup {
            candidate: candidate.clone(),
            manager: self.clone(),
            reservation_token,
            completed: false,
        })
    }

    fn cancel_startup(&self, candidate: &GenerationCandidate, reservation_token: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.cancel_startup_locked(&mut state, candidate, reservation_token);
    }

    fn cancel_startup_locked(
        &self,
        state: &mut GenerationState,
        candidate: &GenerationCandidate,
        reservation_token: u64,
    ) {
        let Some(starting) = state.starting_candidate else {
            return;
        };
        if starting.candidate_id != candidate.id || starting.token != reservation_token {
            return;
        }
        match starting.phase {
            CandidateStartPhase::Starting => {
                state.starting_candidate = None;
            }
            CandidateStartPhase::RuntimeOwned => {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
                self.quarantine_locked(state, candidate, "runtime_start");
            }
            CandidateStartPhase::Activating => {}
        }
    }

    /// Restores the retained previous generation as active.
    ///
    /// # Errors
    ///
    /// Returns an error when no previous generation is retained.
    pub fn rollback(&self) -> Result<GenerationCandidate, GenerationError> {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous_config = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.shutting_down {
                return Err(GenerationError::MutationInProgress);
            }
            let previous = state.previous.clone().ok_or(GenerationError::NoPrevious)?;
            if state.quarantined_revision.as_ref() == Some(&previous.revision.candidate) {
                return Err(GenerationError::QuarantinedRevision);
            }
            previous
        };
        let active_reservations = previous_config.reservations.clone();
        let document = CanonicalConfigDocument {
            disk_revision: previous_config.revision.disk.clone(),
            candidate_revision: previous_config.revision.candidate.clone(),
            normalized_config: (*previous_config.config).clone(),
            format: ConfigFormat::Kdl,
            compositional: false,
            dependencies: Vec::new(),
            config_preview: String::new(),
            diagnostics: Vec::new(),
        };
        let prepared = PreparedGeneration::prepare(
            document,
            Some(&active_reservations),
            self.process.clone(),
        )?;
        let candidate = GenerationCandidate {
            generation: Arc::new(RuntimeGeneration::activate(prepared)),
            id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
        };
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate = Some(candidate.clone());
        self.counters.rollbacks.fetch_add(1, Ordering::Relaxed);
        crate::operational_event::emit(
            "generation_rollback",
            "prepared",
            Some(&candidate.revision().candidate),
        );
        Ok(candidate)
    }

    pub fn quarantine(&self, candidate: &GenerationCandidate, failure: &'static str) {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.candidate.as_ref().is_some_and(|pending| {
            pending.id == candidate.id && Arc::ptr_eq(&pending.generation, &candidate.generation)
        }) {
            if let Some(starting) = state
                .starting_candidate
                .filter(|starting| starting.candidate_id == candidate.id)
            {
                if starting.phase != CandidateStartPhase::Starting {
                    candidate.generation.cancel_runtime_start();
                }
                if starting.phase != CandidateStartPhase::Activating {
                    state.starting_candidate = None;
                }
            }
            self.quarantine_locked(&mut state, candidate, failure);
        }
    }

    fn quarantine_locked(
        &self,
        state: &mut GenerationState,
        candidate: &GenerationCandidate,
        failure: &'static str,
    ) {
        if !candidate_matches(state.candidate.as_ref(), candidate) {
            return;
        }
        state.candidate = None;
        state.quarantined_revision = Some(candidate.revision().candidate.clone());
        state.last_failure = Some(failure);
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        crate::operational_event::emit(
            "generation_start",
            "quarantined",
            Some(&candidate.revision().candidate),
        );
    }

    #[must_use]
    pub fn candidate(&self) -> Option<GenerationCandidate> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate
            .clone()
    }

    pub(crate) fn observe_disk_revision(&self, revision: ConfigRevision) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .disk_revision = Some(revision);
    }

    /// Acquires an active-generation permit that prevents publication until it is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when no generation is active or the expected revision is stale.
    pub fn begin_mutation(
        &self,
        expected_revision: &str,
    ) -> Result<GenerationMutation, GenerationError> {
        self.begin_active_mutation(Some(expected_revision))
    }

    /// Acquires the shared active-generation permit required before mutating canonical config.
    ///
    /// # Errors
    ///
    /// Returns an error when shutdown or generation publication prevents a new mutation.
    pub fn begin_config_mutation(&self) -> Result<GenerationMutation, GenerationError> {
        self.begin_active_mutation(None)
    }

    fn begin_active_mutation(
        &self,
        expected_revision: Option<&str>,
    ) -> Result<GenerationMutation, GenerationError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            return Err(GenerationError::MutationInProgress);
        }
        let active = state.active.as_ref().ok_or(GenerationError::NoActive)?;
        if state.starting_candidate.is_some() {
            return Err(GenerationError::MutationInProgress);
        }
        if expected_revision.is_some_and(|expected| active.revision.candidate.as_str() != expected)
        {
            return Err(GenerationError::RevisionConflict);
        }
        active.mutations.fetch_add(1, Ordering::AcqRel);
        Ok(GenerationMutation {
            generation: Arc::clone(active),
        })
    }

    #[must_use]
    pub fn active(&self) -> Option<Arc<RuntimeGeneration>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .clone()
    }

    #[must_use]
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let generations = self.reserve_shutdown();
        let mut recorder_shutdowns = Self::initiate_recorder_shutdown(&generations, deadline);
        self.collect_recorder_cleanups(deadline, &mut recorder_shutdowns);
        let mut drained = true;
        for generation in &generations {
            let remaining = deadline.saturating_duration_since(Instant::now());
            drained &= generation.drain(remaining);
        }
        for shutdown in &recorder_shutdowns {
            if !shutdown.wait_until(deadline) {
                return false;
            }
        }
        drained
    }

    /// Establishes terminal admission and returns recorder completion handles without waiting.
    #[must_use]
    pub fn begin_shutdown(&self, deadline: Instant) -> Vec<RtmpRecorderShutdown> {
        let generations = self.reserve_shutdown();
        let mut shutdowns = Self::initiate_recorder_shutdown(&generations, deadline);
        self.collect_recorder_cleanups(deadline, &mut shutdowns);
        shutdowns
    }

    fn collect_recorder_cleanups(
        &self,
        deadline: Instant,
        shutdowns: &mut Vec<RtmpRecorderShutdown>,
    ) {
        let lifecycles = {
            let mut cleanup = self
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cleanup
                .recorder_lifecycles
                .retain(|lifecycle| !lifecycle.is_complete());
            cleanup.recorder_lifecycles.clone()
        };
        for lifecycle in &lifecycles {
            push_unique_shutdown(shutdowns, &lifecycle.initiate_shutdown(deadline));
        }
    }

    fn reserve_shutdown(&self) -> Vec<Arc<RuntimeGeneration>> {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.shutting_down = true;
        let mut generations = Vec::with_capacity(3);
        if let Some(active) = &state.active {
            push_unique_generation(&mut generations, active);
        }
        if let Some(previous) = &state.previous {
            push_unique_generation(&mut generations, previous);
        }
        let starting = state.starting_candidate;
        if let Some(candidate) = state.candidate.take() {
            if starting
                .is_some_and(|reservation| reservation.phase != CandidateStartPhase::Starting)
            {
                candidate.generation.cancel_runtime_start();
            }
            push_unique_generation(&mut generations, &candidate.generation);
        }
        if starting.is_none_or(|reservation| reservation.phase != CandidateStartPhase::Activating) {
            state.starting_candidate = None;
        }
        for generation in generations {
            push_unique_generation(&mut state.shutdown_generations, &generation);
        }
        state.shutdown_generations.clone()
    }

    fn initiate_recorder_shutdown(
        generations: &[Arc<RuntimeGeneration>],
        deadline: Instant,
    ) -> Vec<RtmpRecorderShutdown> {
        generations
            .iter()
            .flat_map(|generation| generation.initiate_recorder_shutdown(deadline))
            .collect()
    }

    #[must_use]
    pub fn status(&self) -> GenerationStatus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        GenerationStatus {
            build_version: crate::cli::BUILD_VERSION,
            disk_revision: state.disk_revision.clone(),
            candidate_revision: state
                .candidate
                .as_ref()
                .map(|candidate| candidate.revision().candidate.clone()),
            active_revision: state
                .active
                .as_ref()
                .map(|active| active.revision.candidate.clone()),
            previous_revision: state
                .previous
                .as_ref()
                .map(|previous| previous.revision.candidate.clone()),
            quarantined_revision: state.quarantined_revision.clone(),
            active_accepting: state
                .active
                .as_ref()
                .is_some_and(|active| active.accepting()),
            degraded: state.last_failure.is_some(),
            last_failure: state.last_failure,
            prepares: self.counters.prepares.load(Ordering::Relaxed),
            activations: self.counters.activations.load(Ordering::Relaxed),
            failures: self.counters.failures.load(Ordering::Relaxed),
            rollbacks: self.counters.rollbacks.load(Ordering::Relaxed),
        }
    }
}

fn candidate_matches(
    pending: Option<&GenerationCandidate>,
    candidate: &GenerationCandidate,
) -> bool {
    pending.is_some_and(|pending| {
        pending.id == candidate.id && Arc::ptr_eq(&pending.generation, &candidate.generation)
    })
}

fn clear_starting_reservation(
    state: &mut GenerationState,
    candidate_id: u64,
    token: u64,
    phase: CandidateStartPhase,
) {
    if state.starting_candidate
        == Some(CandidateStartReservation {
            candidate_id,
            phase,
            token,
        })
    {
        state.starting_candidate = None;
    }
}

fn active_matches_reservation(
    state: &GenerationState,
    reservation: &ActivationReservation,
) -> bool {
    match (&state.active, &reservation.previous) {
        (Some(active), Some(previous)) => {
            Arc::ptr_eq(active, previous)
                && Some(&active.revision.candidate) == reservation.previous_revision.as_ref()
        }
        (None, None) => reservation.previous_revision.is_none(),
        (Some(_), None) | (None, Some(_)) => false,
    }
}

fn push_unique_generation(
    generations: &mut Vec<Arc<RuntimeGeneration>>,
    generation: &Arc<RuntimeGeneration>,
) {
    if !generations
        .iter()
        .any(|existing| Arc::ptr_eq(existing, generation))
    {
        generations.push(Arc::clone(generation));
    }
}

fn push_unique_shutdown(
    shutdowns: &mut Vec<RtmpRecorderShutdown>,
    shutdown: &RtmpRecorderShutdown,
) {
    if !shutdowns
        .iter()
        .any(|existing| existing.is_same_lifecycle(shutdown))
    {
        shutdowns.push(shutdown.clone());
    }
}

fn push_unique_lifecycle(
    lifecycles: &mut Vec<RtmpRecorderLifecycle>,
    lifecycle: RtmpRecorderLifecycle,
) {
    if !lifecycles
        .iter()
        .any(|existing| existing.is_same_lifecycle(&lifecycle))
    {
        lifecycles.push(lifecycle);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationStatus {
    pub build_version: &'static str,
    pub disk_revision: Option<ConfigRevision>,
    pub candidate_revision: Option<ConfigRevision>,
    pub active_revision: Option<ConfigRevision>,
    pub previous_revision: Option<ConfigRevision>,
    pub quarantined_revision: Option<ConfigRevision>,
    pub active_accepting: bool,
    pub degraded: bool,
    pub last_failure: Option<&'static str>,
    pub prepares: u64,
    pub activations: u64,
    pub failures: u64,
    pub rollbacks: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("candidate runtime preparation failed: {0}")]
    Plan(#[source] Box<crate::ServicePlanError>),
    #[error("candidate runtime preparation failed")]
    RuntimePrepare,
    #[error("candidate listener reservation failed")]
    Listener(#[from] io::Error),
    #[error("candidate metrics preparation failed")]
    Metrics(#[from] crate::MetricsError),
    #[error("candidate RTMP preparation failed")]
    Rtmp(#[from] crate::ServicePlanError),
    #[error("candidate management token preparation failed")]
    ManagementToken,
    #[error("no prepared candidate is available")]
    NoCandidate,
    #[error("the prepared candidate was superseded")]
    CandidateSuperseded,
    #[error("an active-generation mutation is in progress")]
    MutationInProgress,
    #[error("active generation acceptors did not quiesce")]
    AcceptorQuiesce,
    #[error("no active generation is available")]
    NoActive,
    #[error("the active generation revision changed")]
    RevisionConflict,
    #[error("no previous generation is available")]
    NoPrevious,
    #[error("the previous generation revision is quarantined")]
    QuarantinedRevision,
}

impl GenerationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Plan(_) => "service_plan_prepare",
            Self::RuntimePrepare => "runtime_prepare",
            Self::Listener(_) => "listener_reservation",
            Self::Metrics(_) => "metrics_prepare",
            Self::Rtmp(_) => "rtmp_prepare",
            Self::ManagementToken => "management_token_prepare",
            Self::NoCandidate => "candidate_missing",
            Self::CandidateSuperseded => "candidate_superseded",
            Self::MutationInProgress => "mutation_in_progress",
            Self::AcceptorQuiesce => "acceptor_quiesce",
            Self::NoActive => "generation_unavailable",
            Self::RevisionConflict => "generation_conflict",
            Self::NoPrevious => "previous_missing",
            Self::QuarantinedRevision => "generation_quarantined",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        },
        thread,
    };

    use oxiroute_config::render_lua;
    use tempfile::TempDir;

    use crate::config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome};

    use super::*;

    fn document() -> CanonicalConfigDocument {
        document_with_max_connections(None)
    }

    fn document_with_max_connections(max_connections: Option<u64>) -> CanonicalConfigDocument {
        document_for(&Config {
            version: 1,
            max_connections,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        })
    }

    fn document_for(config: &Config) -> CanonicalConfigDocument {
        let directory = TempDir::new().expect("directory");
        let path = directory.path().join("oxiroute.lua");
        fs::write(&path, render_lua(config).expect("render")).expect("write");
        let ConfigLoadOutcome::Loaded(document) = CanonicalConfigCoordinator::new(path)
            .expect("coordinator")
            .load()
        else {
            panic!("load")
        };
        *document
    }

    fn claim_and_mark_runtime_started(startup: &mut GenerationStartup) -> Arc<RuntimeGeneration> {
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        assert!(generation.mark_runtime_started());
        generation
    }

    fn pause_activation_at(
        manager: &GenerationManager,
        point: ActivationHookPoint,
    ) -> (mpsc::Receiver<()>, Arc<Barrier>) {
        let (paused, paused_rx) = mpsc::sync_channel(1);
        let release = Arc::new(Barrier::new(2));
        let hook_release = Arc::clone(&release);
        let notified = Arc::new(AtomicBool::new(false));
        manager.set_activation_hook(Arc::new(move |observed| {
            if observed == point && !notified.swap(true, Ordering::AcqRel) {
                paused.send(()).expect("activation pause receiver");
                hook_release.wait();
            }
        }));
        (paused_rx, release)
    }

    #[test]
    fn activation_rollback_and_failed_activation_preserve_published_state() {
        let manager = GenerationManager::new();
        let first_candidate = manager.prepare(document()).expect("prepare first");
        let first = manager.activate(&first_candidate).expect("activate first");
        let second_candidate = manager.prepare(document()).expect("prepare second");
        let second = manager
            .activate(&second_candidate)
            .expect("activate second");

        assert!(!first.accepting());
        assert!(second.accepting());
        let rollback = manager.rollback().expect("rollback");
        assert_eq!(rollback.revision(), first.revision());
        assert!(second.accepting());
    }

    #[test]
    fn third_activation_keeps_only_the_immediately_previous_revision_rollbackable() {
        let manager = GenerationManager::new();
        let first = manager
            .prepare(document_with_max_connections(Some(1)))
            .expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        manager.activate(&second).expect("second activation");
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");
        manager.activate(&third).expect("third activation");

        let rollback = manager.rollback().expect("rollback candidate");

        assert_eq!(rollback.revision(), second.revision());
        assert_ne!(rollback.revision(), first.revision());
    }

    #[test]
    fn bounded_drain_waits_for_all_protocol_reference_kinds() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("prepare");
        let generation = manager.activate(&candidate).expect("activate");
        let references = [
            RuntimeReferenceKind::ForwardHttp1,
            RuntimeReferenceKind::Http1,
            RuntimeReferenceKind::Http2,
            RuntimeReferenceKind::WebSocket,
            RuntimeReferenceKind::Tcp,
            RuntimeReferenceKind::Rtmp,
        ]
        .map(|kind| generation.begin_reference(kind).expect("reference"));
        let draining = Arc::clone(&generation);
        let waiter = thread::spawn(move || draining.drain(Duration::from_secs(1)));
        thread::sleep(Duration::from_millis(20));
        assert!(!generation.accepting());
        drop(references);
        assert!(waiter.join().expect("drain thread"));
    }

    #[test]
    fn failed_listener_candidate_releases_resources_and_preserves_active_generation() {
        let manager = GenerationManager::new();
        let active_candidate = manager.prepare(document()).expect("prepare active");
        let active = manager.activate(&active_candidate).expect("activate");
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied");
        let address = occupied.local_addr().expect("address");
        let mut candidate = (**active.config()).clone();
        candidate.listeners.push(oxiroute_config::Listener {
            name: "live".into(),
            bind: oxiroute_config::ListenerBind::Socket { address },
            protocol: oxiroute_config::Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        candidate.rtmp_services.push(oxiroute_config::RtmpService {
            name: "live".into(),
            outbound_chunk_size: 4_096,
            access_log: None,
            applications: vec![oxiroute_config::RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: false,
                push_targets: Vec::new(),
                fanout: oxiroute_config::RtmpFanoutPolicy::default(),
                recorders: Vec::new(),
            }],
        });

        assert!(manager.prepare(document_for(&candidate)).is_err());
        assert!(Arc::ptr_eq(
            &manager.active().expect("still active"),
            &active
        ));
        assert!(active.accepting());
        assert!(manager.candidate().is_none());
    }

    #[test]
    fn superseded_candidate_cannot_activate_a_newer_preparation() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let second = manager.prepare(document()).expect("second candidate");

        assert!(matches!(
            manager.activate(&first),
            Err(GenerationError::CandidateSuperseded)
        ));
        let active = manager.activate(&second).expect("second activation");
        assert!(Arc::ptr_eq(&active, second.generation()));
    }

    #[test]
    fn publication_moves_the_admission_gate_without_overlap() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager.prepare(document()).expect("second candidate");

        assert!(first.generation().begin_admission().is_some());
        assert!(second.generation().begin_admission().is_none());
        manager.activate(&second).expect("second activation");
        assert!(first.generation().begin_admission().is_none());
        assert!(second.generation().begin_admission().is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publication_waits_for_acceptor_quiescence() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let second = manager.prepare(document()).expect("second candidate");
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = std::sync::mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            let result = activation_manager
                .activate(&activation_candidate)
                .map(|activated| Arc::ptr_eq(&activated, activation_candidate.generation()))
                .map_err(|error| error.code());
            activated_tx.send(result).expect("activation receiver");
        });

        let state = acceptor.changed().await.expect("gate close notification");
        assert!(!state.accepting);
        assert!(!second.generation().accepting());
        assert!(activated_rx.try_recv().is_err());

        acceptor.acknowledge(state.epoch);
        assert!(activated_rx.try_recv().is_err());
        drop(ownership);

        let activated_second = activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result")
            .expect("second activation");
        assert!(activated_second);
        assert!(second.generation().accepting());
        activation.join().expect("activation thread");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_remains_readable_on_a_single_thread_runtime_during_publication() {
        let manager = GenerationManager::new();
        let first = manager
            .prepare(document_with_max_connections(Some(1)))
            .expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let (activation_paused, release_activation) =
            pause_activation_at(&manager, ActivationHookPoint::GateClosed);
        let reads = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let reader_manager = manager.clone();
        let reader_reads = Arc::clone(&reads);
        let reader_stop = Arc::clone(&stop);
        let reader = tokio::spawn(async move {
            while !reader_stop.load(Ordering::Acquire) {
                let _ = reader_manager.status();
                reader_reads.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        });
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });

        activation_paused
            .recv_timeout(Duration::from_secs(1))
            .expect("activation did not reach closed gate");
        let gate_state = tokio::time::timeout(Duration::from_secs(1), acceptor.changed())
            .await
            .expect("gate close timeout")
            .expect("gate close");
        let reads_at_close = reads.load(Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(1), async {
            while reads.load(Ordering::Relaxed) <= reads_at_close {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("status reader did not progress while publication was paused");
        let status = manager.status();
        assert_eq!(
            status.active_revision,
            Some(first.revision().candidate.clone())
        );
        assert_eq!(
            status.candidate_revision,
            Some(second.revision().candidate.clone())
        );
        assert!(!status.active_accepting);

        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);
        release_activation.wait();
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result")
            .expect("activation");
        stop.store(true, Ordering::Release);
        reader.await.expect("status reader");
        activation.join().expect("activation thread");
    }

    #[test]
    fn shutdown_close_before_activation_close_prevents_publication() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let (activation_paused, release_activation) =
            pause_activation_at(&manager, ActivationHookPoint::Reserved);
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });
        activation_paused
            .recv_timeout(Duration::from_secs(1))
            .expect("activation reservation pause");

        assert!(manager.shutdown(Duration::ZERO));
        release_activation.wait();

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::MutationInProgress.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(!first.generation().accepting());
        assert!(!second.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().activations, 1);
    }

    #[test]
    fn shutdown_during_closed_gate_prevents_activation_commit() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let (activation_paused, release_activation) =
            pause_activation_at(&manager, ActivationHookPoint::GateClosed);
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });
        activation_paused
            .recv_timeout(Duration::from_secs(1))
            .expect("closed gate pause");

        assert!(manager.shutdown(Duration::ZERO));
        release_activation.wait();

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::MutationInProgress.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(!first.generation().accepting());
        assert!(!second.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().activations, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_releases_manager_state_while_owned_generations_drain() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let shutdown_manager = manager.clone();
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let shutdown = thread::spawn(move || {
            shutdown_tx
                .send(shutdown_manager.shutdown(Duration::from_secs(1)))
                .expect("shutdown receiver");
        });

        let gate_state = tokio::time::timeout(Duration::from_secs(1), acceptor.changed())
            .await
            .expect("shutdown gate timeout")
            .expect("shutdown gate close");
        let status = manager.status();
        assert_eq!(
            status.active_revision,
            Some(first.revision().candidate.clone())
        );
        assert!(!status.active_accepting);
        assert!(status.candidate_revision.is_none());
        assert!(matches!(
            manager.activate(&second),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(second.generation().runtime_failed());

        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);
        assert!(
            shutdown_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("shutdown result")
        );
        shutdown.join().expect("shutdown thread");
        drop(startup);
    }

    #[test]
    fn terminal_shutdown_rejects_new_mutations_but_keeps_validation_read_only() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let active = manager.activate(&first).expect("first activation");
        let pending = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("pending candidate");

        drop(manager.begin_shutdown(Instant::now() + Duration::from_secs(1)));

        assert!(matches!(
            manager.begin_mutation(active.revision().candidate.as_str()),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.begin_config_mutation(),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.begin_candidate_start(&pending),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.prepare(document()),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.rollback(),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(manager.validate_candidate(document()).is_ok());
    }

    #[test]
    fn repeated_shutdown_retains_active_previous_and_removed_candidate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        manager.activate(&second).expect("second activation");
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");

        let first_shutdown = manager.reserve_shutdown();
        let second_shutdown = manager.reserve_shutdown();

        assert_eq!(first_shutdown.len(), 3);
        assert_eq!(second_shutdown.len(), 3);
        for expected in [second.generation(), first.generation(), third.generation()] {
            assert!(
                first_shutdown
                    .iter()
                    .any(|generation| Arc::ptr_eq(generation, expected))
            );
            assert!(
                second_shutdown
                    .iter()
                    .any(|generation| Arc::ptr_eq(generation, expected))
            );
        }
        assert!(manager.candidate().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn candidate_can_be_superseded_while_the_gate_is_quiescing() {
        let manager = GenerationManager::new();
        let first = manager
            .prepare(document_with_max_connections(Some(1)))
            .expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");
        assert!(candidate_matches(manager.candidate().as_ref(), &third));
        assert!(matches!(
            manager.activate(&third),
            Err(GenerationError::MutationInProgress)
        ));
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::CandidateSuperseded.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(!second.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &third));
        assert_eq!(manager.status().failures, 0);
        drop(acceptor);
        let active = manager.activate(&third).expect("third activation");
        assert!(Arc::ptr_eq(&active, third.generation()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_activation_is_rejected_without_touching_the_gate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        assert!(matches!(
            manager.activate(&second),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(activated_rx.try_recv().is_err());
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result")
            .expect("activation");
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            second.generation()
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quarantine_during_publication_cancels_once_and_reopens_the_active_gate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(startup.activate().map(|_| ()).map_err(|error| error.code()))
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        manager.quarantine(&second, "runtime_start");
        manager.quarantine(&second, "runtime_start");
        assert_eq!(manager.status().failures, 1);
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::CandidateSuperseded.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().failures, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_failure_during_publication_is_quarantined_without_retry() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(startup.activate().map(|_| ()).map_err(|error| error.code()))
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        second.generation().mark_runtime_failed();
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::RuntimePrepare.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().failures, 1);
        assert!(matches!(
            manager.begin_candidate_start(&second),
            Err(GenerationError::CandidateSuperseded)
        ));
    }

    #[test]
    fn supersession_precedes_runtime_failure_and_quiesce_timeout() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let reservation = manager
            .reserve_activation(&second, None)
            .expect("activation reservation");
        let close = first.generation().accept_gate().close();
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");
        second.generation().mark_runtime_failed();

        assert!(matches!(
            manager.publish_reserved_candidate(&second, &reservation, Some(close), false),
            Err(GenerationError::CandidateSuperseded)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &third));
        assert_eq!(manager.status().failures, 0);
    }

    #[test]
    fn runtime_failure_precedes_quiesce_timeout_and_uses_runtime_start_quarantine() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let reservation = manager
            .reserve_activation(&second, Some(startup.reservation_token))
            .expect("activation reservation");
        let close = first.generation().accept_gate().close();
        second.generation().mark_runtime_failed();

        assert!(matches!(
            manager.publish_reserved_candidate(&second, &reservation, Some(close), false),
            Err(GenerationError::RuntimePrepare)
        ));
        startup.completed = true;
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        let status = manager.status();
        assert_eq!(status.failures, 1);
        assert_eq!(status.last_failure, Some("runtime_start"));
    }

    #[test]
    fn gate_timeout_consumes_a_started_candidate_and_reopens_the_active_generation() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let ownership = first
            .generation()
            .begin_admission()
            .expect("accept ownership");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);

        assert!(matches!(
            startup.activate_with_timeout(Duration::ZERO),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        let status = manager.status();
        assert_eq!(status.failures, 1);
        assert_eq!(
            status.quarantined_revision,
            Some(second.revision().candidate.clone())
        );
        assert!(matches!(
            manager.begin_candidate_start(&second),
            Err(GenerationError::CandidateSuperseded)
        ));
        drop(ownership);
    }

    #[test]
    fn gate_timeout_keeps_an_unstarted_candidate_retryable() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let ownership = first
            .generation()
            .begin_admission()
            .expect("accept ownership");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");

        assert!(matches!(
            manager.activate_inner(&second, None, Duration::ZERO),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(candidate_matches(manager.candidate().as_ref(), &second));
        assert!(first.generation().accepting());
        assert_eq!(manager.status().failures, 0);
        drop(ownership);

        let active = manager
            .activate(&second)
            .expect("retry unstarted candidate");
        assert!(Arc::ptr_eq(&active, second.generation()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_publication_does_not_reopen_an_already_closed_active_gate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut acceptor = first.generation().accept_gate().register();
        first.generation().stop_accepting();
        assert!(!first.generation().accepting());
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");

        assert!(matches!(
            manager.activate_inner(&second, None, Duration::ZERO),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(!first.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &second));
        let gate_state = acceptor.changed().await.expect("gate close");
        acceptor.acknowledge(gate_state.epoch);
    }

    #[test]
    fn publication_failure_does_not_undo_a_later_shutdown_close() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let reservation = manager
            .reserve_activation(&second, None)
            .expect("activation reservation");
        let activation_close = first.generation().accept_gate().close();
        first.generation().stop_accepting();

        assert!(matches!(
            manager.publish_reserved_candidate(
                &second,
                &reservation,
                Some(activation_close),
                false,
            ),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(!first.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &second));
    }

    #[test]
    fn cancelling_a_claimed_start_rejects_a_late_marker_and_consumes_the_candidate_once() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("candidate");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        assert!(matches!(
            startup.claim_runtime_start(),
            Err(GenerationError::CandidateSuperseded)
        ));
        let marker_generation = Arc::clone(&generation);
        let (marker_ready, start_marker) = mpsc::sync_channel(0);
        let marker = thread::spawn(move || {
            start_marker.recv().expect("marker release");
            marker_generation.mark_runtime_started()
        });

        drop(startup);
        marker_ready.send(()).expect("marker receiver");

        assert!(!marker.join().expect("runtime marker"));
        assert!(generation.runtime_failed());
        assert!(!generation.claim_runtime_start());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().failures, 1);
        manager.quarantine(&candidate, "runtime_start");
        assert_eq!(manager.status().failures, 1);
        assert!(matches!(
            manager.begin_candidate_start(&candidate),
            Err(GenerationError::CandidateSuperseded)
        ));
    }

    #[test]
    fn activation_before_runtime_claim_releases_only_the_unstarted_reservation() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("candidate");
        let startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");

        assert!(matches!(
            startup.activate(),
            Err(GenerationError::RuntimePrepare)
        ));
        assert!(candidate_matches(manager.candidate().as_ref(), &candidate));
        assert!(!candidate.generation().runtime_failed());
        assert_eq!(manager.status().failures, 0);

        let mut next_startup = manager
            .begin_candidate_start(&candidate)
            .expect("next startup reservation");
        claim_and_mark_runtime_started(&mut next_startup);
        let active = next_startup.activate().expect("activation");
        assert!(Arc::ptr_eq(&active, candidate.generation()));
    }

    #[test]
    fn stale_startup_token_cannot_cancel_a_new_reservation() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let active = manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let stale = manager
            .begin_candidate_start(&second)
            .expect("first startup reservation");
        let stale_token = stale.reservation_token;
        drop(stale);
        let mut current = manager
            .begin_candidate_start(&second)
            .expect("second startup reservation");

        manager.cancel_startup(&second, stale_token);
        assert!(matches!(
            manager.begin_mutation(active.revision().candidate.as_str()),
            Err(GenerationError::MutationInProgress)
        ));
        claim_and_mark_runtime_started(&mut current);
        let activated = current.activate().expect("current startup activation");
        assert!(Arc::ptr_eq(&activated, second.generation()));
    }

    #[test]
    fn quarantined_candidate_leaves_active_unchanged_and_can_be_prepared_again() {
        let manager = GenerationManager::new();
        let active_candidate = manager.prepare(document()).expect("active candidate");
        let active = manager.activate(&active_candidate).expect("active");
        let failed = manager.prepare(document()).expect("failed candidate");

        manager.quarantine(&failed, "runtime_start");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active retained"),
            &active
        ));
        assert!(active.accepting());
        assert!(manager.status().quarantined_revision.is_some());
        assert!(manager.prepare(document()).is_ok());
    }

    #[test]
    fn failed_rollback_revision_cannot_be_started_again() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut second_config = (*first.generation().config).clone();
        second_config.max_connections = Some(2);
        let second = manager
            .prepare(document_for(&second_config))
            .expect("second candidate");
        manager.activate(&second).expect("second activation");

        let rollback = manager.rollback().expect("rollback candidate");
        manager.quarantine(&rollback, "runtime_start");
        assert!(matches!(
            manager.rollback(),
            Err(GenerationError::QuarantinedRevision)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("second remains active"),
            second.generation()
        ));
    }

    #[test]
    fn stale_revision_cannot_acquire_a_mutation_permit() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("candidate");
        let active = manager.activate(&candidate).expect("active");

        assert!(matches!(
            manager
                .begin_mutation("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
            Err(GenerationError::RevisionConflict)
        ));
        assert!(
            manager
                .begin_mutation(active.revision().candidate.as_str())
                .is_ok()
        );
    }

    #[test]
    fn candidate_start_waits_for_mutation_and_then_excludes_new_mutations_until_publication() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let active = manager.activate(&first).expect("active");
        let second = manager.prepare(document()).expect("second candidate");
        let mutation = manager
            .begin_mutation(active.revision().candidate.as_str())
            .expect("active mutation");

        assert!(matches!(
            manager.begin_candidate_start(&second),
            Err(GenerationError::MutationInProgress)
        ));
        drop(mutation);

        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("reserved candidate startup");
        assert!(matches!(
            manager.begin_mutation(active.revision().candidate.as_str()),
            Err(GenerationError::MutationInProgress)
        ));
        claim_and_mark_runtime_started(&mut startup);
        let activated = startup.activate().expect("reserved activation");
        assert!(Arc::ptr_eq(&activated, second.generation()));
    }
}
