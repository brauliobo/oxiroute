#[allow(clippy::wildcard_imports)]
use super::*;

impl GenerationManager {
    /// Fully prepares a candidate without writing the canonical configuration or publishing it.
    ///
    /// # Errors
    ///
    /// Returns a redacted preparation error. A failed candidate is released and active state is
    /// unchanged.
    pub fn prepare(
        &self,
        document: ResolvedConfigDocument,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_with_deadline_internal(document, None)
    }

    #[doc(hidden)]
    pub fn prepare_with_deadline(
        &self,
        document: ResolvedConfigDocument,
        deadline: Instant,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_with_deadline_internal(document, Some(deadline))
    }

    fn prepare_with_deadline_internal(
        &self,
        document: ResolvedConfigDocument,
        deadline: Option<Instant>,
    ) -> Result<GenerationCandidate, GenerationError> {
        let disk_revision = document.authored_revision.clone();
        let candidate_revision = document.effective_revision.clone();
        let _operation =
            self.acquire_preparation_operation(deadline, &disk_revision, &candidate_revision)?;
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
            .map(|generation| generation.reservations().clone());
        let prepared = PreparedGeneration::prepare_from_with_deadline(
            document,
            ListenerSource::BindOrReuse {
                previous: previous.as_ref(),
                process: self.process.clone(),
            },
            deadline,
        )
        .and_then(|output| {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                drop(output);
                Err(GenerationError::PreparationTimedOut)
            } else {
                Ok(output)
            }
        })
        .map(|output| match output {
            PreparationOutput::Prepared(prepared) => {
                Arc::new(RuntimeGeneration::activate(prepared))
            }
            PreparationOutput::Validated => unreachable!("runtime preparation returned validation"),
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = Some(disk_revision.clone());
        match prepared {
            Ok(_generation) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                let error = GenerationError::PreparationTimedOut;
                crate::observability::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                Err(error)
            }
            Ok(generation) => {
                let candidate = GenerationCandidate {
                    generation,
                    id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
                };
                crate::observability::operational_event::emit(
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
                crate::observability::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                if !matches!(error, GenerationError::PreparationTimedOut) {
                    state.quarantined_revision = Some(candidate_revision);
                }
                state.last_failure = Some(error.code());
                Err(error)
            }
        }
    }

    /// Fully prepares a candidate from listener descriptors exactly adopted for its config.
    ///
    /// # Errors
    ///
    /// Returns a redacted preparation error. A failed candidate is released and active state is
    /// unchanged.
    #[cfg(target_os = "linux")]
    pub fn prepare_adopted(
        &self,
        document: ResolvedConfigDocument,
        descriptors: DescriptorSet,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_adopted_with_deadline_internal(document, descriptors, None)
    }

    #[cfg(target_os = "linux")]
    #[doc(hidden)]
    pub fn prepare_adopted_with_deadline(
        &self,
        document: ResolvedConfigDocument,
        descriptors: DescriptorSet,
        deadline: Instant,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_adopted_with_deadline_internal(document, descriptors, Some(deadline))
    }

    #[cfg(target_os = "linux")]
    fn prepare_adopted_with_deadline_internal(
        &self,
        document: ResolvedConfigDocument,
        descriptors: DescriptorSet,
        deadline: Option<Instant>,
    ) -> Result<GenerationCandidate, GenerationError> {
        let disk_revision = document.authored_revision.clone();
        let candidate_revision = document.effective_revision.clone();
        let _operation =
            self.acquire_preparation_operation(deadline, &disk_revision, &candidate_revision)?;
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutting_down
        {
            return Err(GenerationError::MutationInProgress);
        }
        let prepared = PreparedGeneration::prepare_from_with_deadline(
            document,
            ListenerSource::Adopt {
                descriptors,
                process: self.process.clone(),
            },
            deadline,
        )
        .and_then(|output| {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                drop(output);
                Err(GenerationError::PreparationTimedOut)
            } else {
                Ok(output)
            }
        })
        .map(|output| match output {
            PreparationOutput::Prepared(prepared) => {
                Arc::new(RuntimeGeneration::activate(prepared))
            }
            PreparationOutput::Validated => unreachable!("runtime preparation returned validation"),
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = Some(disk_revision.clone());
        match prepared {
            Ok(_generation) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                let error = GenerationError::PreparationTimedOut;
                crate::observability::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                Err(error)
            }
            Ok(generation) => {
                let candidate = GenerationCandidate {
                    generation,
                    id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
                };
                crate::observability::operational_event::emit(
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
                crate::observability::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                if !matches!(error, GenerationError::PreparationTimedOut) {
                    state.quarantined_revision = Some(candidate_revision);
                }
                state.last_failure = Some(error.code());
                Err(error)
            }
        }
    }

    /// Preflights RTMP resources and performs temporary listener bind probes without publishing.
    ///
    /// # Errors
    ///
    /// Returns redacted preflight or listener-probe errors. RTMP preflight does not acquire runtime
    /// ownership; listener probes temporarily bind sockets and release them before returning.
    /// Active and pending state are unchanged.
    pub fn validate_candidate(
        &self,
        document: ResolvedConfigDocument,
    ) -> Result<(), GenerationError> {
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            .map(|generation| generation.reservations().clone());
        match PreparedGeneration::prepare_from(
            document,
            ListenerSource::Validate {
                previous: previous.as_ref(),
            },
        )? {
            PreparationOutput::Validated => Ok(()),
            PreparationOutput::Prepared(_) => {
                unreachable!("validation preparation returned a runtime generation")
            }
        }
    }
}
