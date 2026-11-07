#[allow(clippy::wildcard_imports)]
use super::*;

impl GenerationManager {
    /// Restores the retained previous generation as active.
    ///
    /// # Errors
    ///
    /// Returns an error when no previous generation is retained.
    pub fn rollback(&self) -> Result<GenerationCandidate, GenerationError> {
        self.rollback_with_deadline_internal(None)
    }

    pub(crate) fn rollback_with_deadline(
        &self,
        deadline: Instant,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.rollback_with_deadline_internal(Some(deadline))
    }

    fn rollback_with_deadline_internal(
        &self,
        deadline: Option<Instant>,
    ) -> Result<GenerationCandidate, GenerationError> {
        let _operation = self.operations.acquire();
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
        let active_reservations = previous_config.reservations().clone();
        let document = ResolvedConfigDocument {
            authored_revision: previous_config.revision.disk.clone(),
            effective_revision: previous_config.revision.candidate.clone(),
            validated_config: (*previous_config.config).clone(),
            format: ConfigFormat::Kdl,
            compositional: false,
            dependencies: Vec::new(),
            config_preview: String::new(),
            diagnostics: Vec::new(),
        };
        let prepared = PreparedGeneration::prepare_from_with_deadline(
            document,
            ListenerSource::BindOrReuse {
                previous: Some(&active_reservations),
                process: self.process.clone(),
            },
            deadline,
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prepared = match prepared {
            Ok(PreparationOutput::Prepared(prepared))
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) =>
            {
                let error = GenerationError::PreparationTimedOut;
                drop(prepared);
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                return Err(error);
            }
            Ok(PreparationOutput::Prepared(prepared)) => prepared,
            Ok(PreparationOutput::Validated) => {
                unreachable!("rollback preparation returned validation")
            }
            Err(error) => {
                if !matches!(error, GenerationError::PreparationTimedOut) {
                    state.quarantined_revision = Some(previous_config.revision.candidate.clone());
                }
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                return Err(error);
            }
        };
        let candidate = GenerationCandidate {
            generation: Arc::new(RuntimeGeneration::activate(prepared)),
            id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
        };
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let error = GenerationError::PreparationTimedOut;
            drop(candidate);
            self.counters.failures.fetch_add(1, Ordering::Relaxed);
            state.last_failure = Some(error.code());
            return Err(error);
        }
        state.candidate = Some(candidate.clone());
        state.quarantined_revision = None;
        state.last_failure = None;
        self.counters.rollbacks.fetch_add(1, Ordering::Relaxed);
        crate::observability::operational_event::emit(
            "generation_rollback",
            "prepared",
            Some(&candidate.revision().candidate),
        );
        Ok(candidate)
    }

    pub fn quarantine(&self, candidate: &GenerationCandidate, failure: &'static str) {
        let _operation = self.operations.acquire();
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

    pub(super) fn quarantine_locked(
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
        crate::observability::operational_event::emit(
            "generation_start",
            "quarantined",
            Some(&candidate.revision().candidate),
        );
    }

    #[must_use]
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let generations = self.reserve_shutdown();
        let mut recorder_shutdowns = Self::initiate_rtmp_shutdown(&generations, deadline);
        self.collect_recorder_cleanups(deadline, &mut recorder_shutdowns);
        let mut drained = true;
        for generation in &generations {
            let remaining = deadline.saturating_duration_since(Instant::now());
            drained &= generation.drain(remaining);
        }
        for shutdown in &recorder_shutdowns {
            drained &= shutdown.wait_until(deadline);
        }
        self.prune_completed();
        drained
    }

    /// Establishes terminal admission and returns recorder completion handles without waiting.
    #[must_use]
    pub fn begin_shutdown(&self, deadline: Instant) -> Vec<RtmpShutdown> {
        let generations = self.reserve_shutdown();
        let mut shutdowns = Self::initiate_rtmp_shutdown(&generations, deadline);
        self.collect_recorder_cleanups(deadline, &mut shutdowns);
        shutdowns
    }

    fn collect_recorder_cleanups(&self, deadline: Instant, shutdowns: &mut Vec<RtmpShutdown>) {
        let retirement_work = {
            let mut retirements = self
                .rtmp_retirements
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retirements.prune_completed();
            retirements.take_shutdown_work()
        };
        for work in retirement_work {
            let shutdown = work.initiate(deadline);
            push_unique_shutdown(shutdowns, &shutdown);
        }
    }

    pub fn prune_completed(&self) {
        self.rtmp_retirements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prune_completed();
    }

    pub(in super::super) fn reserve_shutdown(&self) -> Vec<Arc<RuntimeGeneration>> {
        let _operation = self.operations.acquire();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.shutting_down = true;
        state.retired.close_for_shutdown();
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

    fn initiate_rtmp_shutdown(
        generations: &[Arc<RuntimeGeneration>],
        deadline: Instant,
    ) -> Vec<RtmpShutdown> {
        generations
            .iter()
            .map(|generation| generation.initiate_rtmp_shutdown(deadline))
            .collect()
    }
}
