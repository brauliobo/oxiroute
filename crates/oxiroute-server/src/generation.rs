use std::{
    collections::HashMap,
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_config::Config;
use oxiroute_config_source::ConfigFormat;
use oxiroute_rtmp::{RtmpRegistry, RtmpServiceRuntime};
use pingora::apps::{AcceptGate, AcceptOwnership};
use serde::Serialize;

use crate::{
    ListenerReservations, ProcessRuntime, RuntimeMetrics, RuntimePlan, ServiceKind,
    config_coordinator::{CanonicalConfigDocument, ConfigRevision},
    runtime_plan,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeReferenceKind {
    Http1,
    Http2,
    WebSocket,
    Tcp,
    Rtmp,
}

impl RuntimeReferenceKind {
    const fn index(self) -> usize {
        match self {
            Self::Http1 => 0,
            Self::Http2 => 1,
            Self::WebSocket => 2,
            Self::Tcp => 3,
            Self::Rtmp => 4,
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
    references: [AtomicU64; 5],
    mutations: AtomicU64,
    reservations: ListenerReservations,
    revision: GenerationRevision,
    rtmp_registry: Arc<RtmpRegistry>,
    rtmp_runtimes: HashMap<String, RtmpServiceRuntime>,
    runtime_started: AtomicBool,
    runtime_failed: AtomicBool,
}

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
            runtime_started: AtomicBool::new(false),
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

    pub fn mark_runtime_started(&self) {
        self.runtime_started.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn runtime_started(&self) -> bool {
        self.runtime_started.load(Ordering::Acquire)
    }

    pub fn mark_runtime_failed(&self) {
        self.runtime_failed.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn runtime_failed(&self) -> bool {
        self.runtime_failed.load(Ordering::Acquire)
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
    starting_candidate: Option<u64>,
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
    counters: Arc<GenerationCounters>,
    next_candidate_id: Arc<AtomicU64>,
    operations: Arc<Mutex<()>>,
    process: ProcessRuntime,
    state: Arc<Mutex<GenerationState>>,
}

pub struct GenerationStartup {
    candidate: GenerationCandidate,
    manager: GenerationManager,
    completed: bool,
}

impl GenerationStartup {
    #[must_use]
    pub fn generation(&self) -> &Arc<RuntimeGeneration> {
        self.candidate.generation()
    }

    pub fn activate(mut self) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let activated = self
            .manager
            .activate_inner(&self.candidate, Some(self.candidate.id));
        self.completed = activated.is_ok();
        activated
    }
}

impl Drop for GenerationStartup {
    fn drop(&mut self) {
        if !self.completed {
            self.manager.cancel_startup(self.candidate.id);
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
            counters: Arc::new(GenerationCounters::default()),
            next_candidate_id: Arc::new(AtomicU64::new(0)),
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
    pub fn activate(
        &self,
        candidate: &GenerationCandidate,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        self.activate_inner(candidate, None)
    }

    fn activate_inner(
        &self,
        candidate: &GenerationCandidate,
        startup: Option<u64>,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let _operation = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = state
            .candidate
            .as_ref()
            .filter(|pending| {
                pending.id == candidate.id
                    && Arc::ptr_eq(&pending.generation, &candidate.generation)
            })
            .ok_or(GenerationError::CandidateSuperseded)?;
        if state.starting_candidate != startup {
            return Err(GenerationError::CandidateSuperseded);
        }
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.mutations.load(Ordering::Acquire) != 0)
        {
            return Err(GenerationError::MutationInProgress);
        }
        let active = Arc::clone(&pending.generation);
        if let Some(previous) = &state.active {
            if !previous.accept_gate.close_and_wait(Duration::from_secs(5)) {
                previous.accept_gate.reopen();
                return Err(GenerationError::AcceptorQuiesce);
            }
        }
        state.candidate = None;
        state.starting_candidate = None;
        if let Some(previous) = state.active.replace(Arc::clone(&active)) {
            state.previous = Some(previous);
        }
        active.start_accepting();
        state.last_failure = None;
        self.counters.activations.fetch_add(1, Ordering::Relaxed);
        crate::operational_event::emit(
            "generation_activate",
            "activated",
            Some(&active.revision.candidate),
        );
        Ok(active)
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
        state.starting_candidate = Some(pending.id);
        Ok(GenerationStartup {
            candidate: candidate.clone(),
            manager: self.clone(),
            completed: false,
        })
    }

    fn cancel_startup(&self, candidate_id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.starting_candidate == Some(candidate_id) {
            state.starting_candidate = None;
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
            if state.starting_candidate == Some(candidate.id) {
                state.starting_candidate = None;
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
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = state.active.as_ref().ok_or(GenerationError::NoActive)?;
        if state.starting_candidate.is_some() {
            return Err(GenerationError::MutationInProgress);
        }
        if active.revision.candidate.as_str() != expected_revision {
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

    pub fn shutdown(&self, timeout: Duration) -> bool {
        let active = self.active();
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .previous
            .clone();
        let started = Instant::now();
        let active_drained = active
            .as_ref()
            .is_none_or(|generation| generation.drain(timeout));
        let remaining = timeout.saturating_sub(started.elapsed());
        let previous_drained = previous
            .as_ref()
            .is_none_or(|generation| generation.drain(remaining));
        active_drained && previous_drained
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
    use std::{fs, thread};

    use oxiroute_config::render_lua;
    use tempfile::TempDir;

    use crate::config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome};

    use super::*;

    fn document() -> CanonicalConfigDocument {
        document_for(&Config {
            version: 1,
            max_connections: None,
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
    fn bounded_drain_waits_for_all_protocol_reference_kinds() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("prepare");
        let generation = manager.activate(&candidate).expect("activate");
        let references = [
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

        let startup = manager
            .begin_candidate_start(&second)
            .expect("reserved candidate startup");
        assert!(matches!(
            manager.begin_mutation(active.revision().candidate.as_str()),
            Err(GenerationError::MutationInProgress)
        ));
        let activated = startup.activate().expect("reserved activation");
        assert!(Arc::ptr_eq(&activated, second.generation()));
    }
}
