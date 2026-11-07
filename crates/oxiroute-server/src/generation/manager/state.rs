#[allow(clippy::wildcard_imports)]
use super::*;

impl GenerationManager {
    #[must_use]
    pub fn candidate(&self) -> Option<GenerationCandidate> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate
            .clone()
    }

    /// Remembers that the configuration on disk was refused, until a later one is accepted.
    ///
    /// Repeating the same refusal is not a new one: it neither raises the counter nor emits
    /// another event. Returns whether this refusal is new.
    pub fn reject_configuration(&self, rejection: ConfigRejection) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(revision) = rejection
            .revision
            .as_ref()
            .and_then(|revision| revision.parse().ok())
        {
            state.disk_revision = Some(revision);
        }
        let repeated = state.last_rejection.as_ref().is_some_and(|last| {
            last.revision == rejection.revision && last.diagnostics == rejection.diagnostics
        });
        if repeated {
            return false;
        }
        state.last_rejection = Some(rejection);
        drop(state);
        self.counters
            .config_rejections
            .fetch_add(1, Ordering::Relaxed);
        crate::observability::operational_event::emit("configuration_reload", "rejected", None);
        true
    }

    /// Records the supervised generation number this worker serves and the revision it replaced,
    /// which a worker cannot observe itself because each one hosts a single generation.
    pub fn set_supervised_lineage(&self, generation: u64, replaced: Option<EffectiveRevision>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .supervised_lineage = Some(SupervisedLineage {
            generation,
            replaced,
        });
    }

    /// Forgets the recorded refusal because the configuration on disk is acceptable again.
    pub fn accept_configuration(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_rejection = None;
    }

    pub(crate) fn observe_disk_revision(&self, revision: AuthoredRevision) {
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
        expected_revision: &EffectiveRevision,
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
        expected_revision: Option<&EffectiveRevision>,
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
        if expected_revision.is_some_and(|expected| active.revision.candidate != *expected) {
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
    pub fn status(&self) -> GenerationStatus {
        self.prune_completed();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        state.retired.sweep_drained(now);
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
                .map(|previous| previous.revision.candidate.clone())
                .or_else(|| state.supervised_lineage.as_ref()?.replaced.clone()),
            generation: state.supervised_lineage.as_ref().map_or_else(
                || self.counters.activations.load(Ordering::Relaxed),
                |lineage| lineage.generation,
            ),
            quarantined_revision: state.quarantined_revision.clone(),
            active_accepting: state
                .active
                .as_ref()
                .is_some_and(|active| active.accepting()),
            degraded: state.last_failure.is_some() || state.last_rejection.is_some(),
            last_failure: state.last_failure,
            last_rejection: state.last_rejection.clone(),
            prepares: self.counters.prepares.load(Ordering::Relaxed),
            activations: self.counters.activations.load(Ordering::Relaxed),
            config_rejections: self.counters.config_rejections.load(Ordering::Relaxed),
            failures: self.counters.failures.load(Ordering::Relaxed),
            rollbacks: self.counters.rollbacks.load(Ordering::Relaxed),
            retired_generations: state
                .retired
                .statuses(now)
                .into_iter()
                .chain(
                    state
                        .remote_retired
                        .iter()
                        .map(|(revision, report, received)| {
                            report.status(revision, now.saturating_duration_since(*received))
                        }),
                )
                .collect(),
            max_retired_generations: state
                .active
                .as_ref()
                .map_or(DEFAULT_MAX_RETIRED_GENERATIONS, |active| {
                    active.config().retired_generation_limit()
                }),
            pending_apply: state
                .retired
                .pending()
                .filter(|pending| {
                    state
                        .candidate
                        .as_ref()
                        .is_some_and(|candidate| candidate.revision().candidate == pending.revision)
                })
                .cloned(),
        }
    }

    /// Replaces the list of retired workers that a supervising master reports.
    ///
    /// # Errors
    ///
    /// Returns an error when a report names a revision that is not a configuration revision.
    pub fn set_remote_retired(
        &self,
        reports: Vec<RemoteRetired>,
    ) -> Result<(), RevisionParseError> {
        let received = Instant::now();
        let reports = reports
            .into_iter()
            .map(|report| Ok((report.revision.parse()?, report, received)))
            .collect::<Result<_, RevisionParseError>>()?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remote_retired = reports;
        Ok(())
    }

    /// Marks retired generations past their drain deadline as closing and returns each once, for
    /// the runtime owner to tear down.
    #[must_use]
    pub fn expire_retired(&self) -> Vec<Arc<RuntimeGeneration>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        state.retired.sweep_drained(now);
        state.retired.expire_due(now)
    }

    /// Whether the candidate must wait for a retired generation to finish before it may activate.
    ///
    /// The limit is the candidate's own `max_retired_generations`.
    #[must_use]
    pub fn defer_for_retired_cap(&self, candidate: &GenerationCandidate) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retired.sweep_drained(Instant::now());
        let limit = candidate.generation().config().retired_generation_limit();
        state.retired.defer(&candidate.revision().candidate, limit)
    }

    #[must_use]
    pub fn retirement_counters(&self) -> RetirementCounters {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired
            .counters()
    }
}
