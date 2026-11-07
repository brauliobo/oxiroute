#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub struct GenerationManager {
    #[cfg(test)]
    activation_hook: Arc<Mutex<Option<ActivationHook>>>,
    counters: Arc<GenerationCounters>,
    rtmp_retirements: Arc<Mutex<RtmpRetirementRegistry>>,
    next_candidate_id: Arc<AtomicU64>,
    next_reservation_token: Arc<AtomicU64>,
    pub(super) operations: Arc<OperationGate>,
    pub(super) process: ProcessRuntime,
    pub(super) registry: Arc<crate::registry::host::RegistryHost>,
    state: Arc<Mutex<GenerationState>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ActivationHookPoint {
    Reserved,
    GateClosed,
}

#[cfg(test)]
type ActivationHook = Arc<dyn Fn(ActivationHookPoint) + Send + Sync>;

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
            rtmp_retirements: Arc::new(Mutex::new(RtmpRetirementRegistry::default())),
            next_candidate_id: Arc::new(AtomicU64::new(0)),
            next_reservation_token: Arc::new(AtomicU64::new(0)),
            operations: Arc::new(OperationGate::default()),
            process: ProcessRuntime::new(None),
            registry: Arc::new(crate::registry::host::RegistryHost::new(true)),
            state: Arc::new(Mutex::new(GenerationState::default())),
        }
    }

    /// Creates a generation manager for a worker using authenticated listener adoption.
    #[must_use]
    pub fn new_supervised() -> Self {
        let mut manager = Self::new();
        manager.process = ProcessRuntime::supervised(None);
        manager.registry = Arc::new(crate::registry::host::RegistryHost::new(false));
        manager
    }

    /// The registration service, once a generation with a `registry` block has been activated.
    #[must_use]
    pub fn registry(&self) -> Option<Arc<crate::registry::service::RegistryService>> {
        self.registry.service()
    }

    /// Lets this process persist and expire registrations, or only follow them.
    pub fn set_registry_writer(&self, writer: bool) {
        self.registry.set_writer(writer);
    }

    fn acquire_preparation_operation(
        &self,
        deadline: Option<Instant>,
        disk_revision: &AuthoredRevision,
        candidate_revision: &EffectiveRevision,
    ) -> Result<OperationAuthority, GenerationError> {
        let operation = match deadline {
            Some(deadline) => self.operations.acquire_until(deadline),
            None => Some(self.operations.acquire()),
        };
        let Some(operation) = operation else {
            let error = GenerationError::PreparationTimedOut;
            crate::observability::operational_event::emit(
                "generation_prepare",
                "rejected",
                Some(candidate_revision),
            );
            self.counters.failures.fetch_add(1, Ordering::Relaxed);
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.disk_revision = Some(disk_revision.clone());
            state.last_failure = Some(error.code());
            return Err(error);
        };
        Ok(operation)
    }

    fn next_reservation_token(&self) -> u64 {
        self.next_reservation_token
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    #[cfg(test)]
    pub(super) fn set_activation_hook(&self, hook: ActivationHook) {
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
}

pub(super) fn candidate_matches(
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

fn push_unique_shutdown(shutdowns: &mut Vec<RtmpShutdown>, shutdown: &RtmpShutdown) {
    if !shutdowns
        .iter()
        .any(|existing| existing.is_same_lifecycle(shutdown))
    {
        shutdowns.push(shutdown.clone());
    }
}

mod activate;
mod prepare;
mod retire;
mod state;

#[cfg(test)]
pub use activate::GenerationStartup;
