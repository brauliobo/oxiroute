#[allow(clippy::wildcard_imports)]
use super::*;

pub struct GenerationStartup {
    candidate: GenerationCandidate,
    manager: GenerationManager,
    pub(in super::super) reservation_token: u64,
    pub(in super::super) completed: bool,
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

    pub(in super::super) fn activate_with_timeout(
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

impl GenerationManager {
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

    pub(in super::super) fn activate_inner(
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

    pub(in super::super) fn reserve_activation(
        &self,
        candidate: &GenerationCandidate,
        startup_token: Option<u64>,
    ) -> Result<ActivationReservation, GenerationError> {
        let _operation = self.operations.acquire();
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

    pub(in super::super) fn publish_reserved_candidate(
        &self,
        candidate: &GenerationCandidate,
        reservation: &ActivationReservation,
        previous_close: Option<AcceptGateClose>,
        quiesced: bool,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let operation = self.operations.acquire();
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
            if previous_current
                && !state.shutting_down
                && let Some(close) = previous_close
            {
                let _ = close.reopen();
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
            active.restore_administrative_states(&previous.administrative_states());
            let evicted = state.previous.replace(previous);
            if let Some(evicted) = &evicted {
                let mut retirements = self
                    .rtmp_retirements
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                retirements.retire(evicted.rtmp_retirement());
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

    fn claim_runtime_start(
        &self,
        candidate: &GenerationCandidate,
        reservation_token: u64,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let _operation = self.operations.acquire();
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
        if candidate.generation.start_rtmp().is_err() {
            candidate.generation.cancel_runtime_start();
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
        let _operation = self.operations.acquire();
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

    pub(in super::super) fn cancel_startup(
        &self,
        candidate: &GenerationCandidate,
        reservation_token: u64,
    ) {
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
}
