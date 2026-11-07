#[allow(clippy::wildcard_imports)]
use super::*;

impl<E: ActionExecutor> Master<E> {
    pub(super) fn handle_phase_failure(
        &mut self,
        role: WorkerRole,
        phase: FailurePhase,
        now: Instant,
    ) -> Result<(), MasterError> {
        match (self.stage, self.replacement_phase(), role) {
            (
                Stage::Operational,
                Some(
                    ReplacementPhase::AdoptingCandidate
                    | ReplacementPhase::Quiescing
                    | ReplacementPhase::ActivatingCandidate,
                ),
                WorkerRole::Candidate,
            )
            | (Stage::Operational, Some(ReplacementPhase::Quiescing), WorkerRole::Active) => {
                self.begin_rollback(phase, now)
            }
            (
                Stage::Operational,
                Some(ReplacementPhase::DrainingRetired | ReplacementPhase::StoppingRetired),
                WorkerRole::Retired,
            ) => self.fail_retired(phase, now),
            (Stage::ShuttingDown { .. }, _, _) => self.force_role(role),
            _ => self.fail_master(phase),
        }
    }

    pub(super) fn handle_disconnect(
        &mut self,
        role: WorkerRole,
        now: Instant,
    ) -> Result<(), MasterError> {
        let instance_id = self
            .worker(role)
            .ok_or(MasterError::MissingWorker(role))?
            .instance_id
            .clone();
        self.events
            .push_back(MasterEvent::WorkerDisconnected { role, instance_id });
        if matches!(
            self.worker(role).map(|worker| worker.state),
            Some(WorkerState::Terminating { .. })
        ) {
            return Ok(());
        }
        self.handle_phase_failure(role, FailurePhase::Disconnected, now)
    }

    pub(super) fn handle_exit(
        &mut self,
        role: WorkerRole,
        status: ExitStatus,
        now: Instant,
    ) -> Result<(), MasterError> {
        let instance_id = self
            .worker(role)
            .ok_or(MasterError::MissingWorker(role))?
            .instance_id
            .clone();
        if self.replacement_phase() == Some(ReplacementPhase::ActivatingCandidate)
            && role == WorkerRole::Active
        {
            self.deferred_active_exit = Some(status);
            self.events.push_back(MasterEvent::WorkerExited {
                role,
                instance_id,
                status,
            });
            return Ok(());
        }
        self.events.push_back(MasterEvent::WorkerExited {
            role,
            instance_id: instance_id.clone(),
            status,
        });
        match (self.stage, self.replacement_phase(), role) {
            (Stage::Failing { .. } | Stage::ShuttingDown { .. }, _, _) => {
                self.take_worker(role);
                Ok(())
            }
            (Stage::Operational, Some(ReplacementPhase::RollingBack), WorkerRole::Candidate) => {
                self.take_worker(role);
                self.apply_replacement_event(
                    ReplacementEvent::CandidateStopped { instance_id },
                    now,
                )?;
                self.maybe_finish_rollback()
            }
            (Stage::Operational, Some(ReplacementPhase::StoppingRetired), WorkerRole::Retired) => {
                self.take_worker(role);
                self.apply_replacement_event(
                    ReplacementEvent::RetiredStopped { instance_id },
                    now,
                )?;
                if self.replacement_failure.is_some() {
                    self.complete_failed_retired()
                } else {
                    self.complete_replacement()
                }
            }
            (
                Stage::Operational,
                Some(
                    ReplacementPhase::AdoptingCandidate
                    | ReplacementPhase::Quiescing
                    | ReplacementPhase::ActivatingCandidate,
                ),
                WorkerRole::Candidate,
            ) => {
                self.begin_rollback(FailurePhase::Crash, now)?;
                self.take_worker(role);
                self.apply_replacement_event(
                    ReplacementEvent::CandidateStopped { instance_id },
                    now,
                )?;
                self.maybe_finish_rollback()
            }
            (Stage::Operational, Some(ReplacementPhase::DrainingRetired), WorkerRole::Retired) => {
                self.fail_retired(FailurePhase::Crash, now)?;
                self.take_worker(role);
                self.apply_replacement_event(
                    ReplacementEvent::RetiredStopped { instance_id },
                    now,
                )?;
                self.complete_failed_retired()
            }
            _ => {
                self.take_worker(role);
                self.fail_master(FailurePhase::Crash)
            }
        }
    }

    fn begin_rollback(&mut self, phase: FailurePhase, now: Instant) -> Result<(), MasterError> {
        if self.deferred_active_exit.is_some() {
            return self.fail_master(FailurePhase::Crash);
        }
        let candidate = self
            .supervisor
            .as_ref()
            .and_then(ReplacementSupervisor::candidate)
            .ok_or(MasterError::MissingWorker(WorkerRole::Candidate))?
            .instance_id
            .clone();
        self.replacement_failure = Some(phase);
        self.events.push_back(MasterEvent::RollbackStarted {
            candidate: candidate.clone(),
            phase,
        });
        self.apply_replacement_event(
            ReplacementEvent::CandidateFailed {
                instance_id: candidate,
            },
            now,
        )?;
        self.maybe_finish_rollback()
    }

    pub(super) fn maybe_finish_rollback(&mut self) -> Result<(), MasterError> {
        if self.replacement_phase() == Some(ReplacementPhase::Running)
            && self.replacement_failure.take().is_some()
        {
            let active = self.active_id()?.clone();
            self.events
                .push_back(MasterEvent::RollbackCompleted { active });
        }
        Ok(())
    }

    pub(super) fn fail_retired(
        &mut self,
        phase: FailurePhase,
        now: Instant,
    ) -> Result<(), MasterError> {
        let retired = self.retired_id()?.clone();
        self.replacement_failure = Some(phase);
        self.apply_replacement_event(
            ReplacementEvent::RetiredFailed {
                instance_id: retired,
            },
            now,
        )
    }

    pub(super) fn complete_failed_retired(&mut self) -> Result<(), MasterError> {
        let phase = self
            .replacement_failure
            .take()
            .ok_or(MasterError::UnexpectedReplacementActions)?;
        self.events.push_back(MasterEvent::RetiredFailed { phase });
        self.events.push_back(MasterEvent::ReplacementCompleted {
            active: self.active_id()?.clone(),
        });
        Ok(())
    }

    fn complete_replacement(&mut self) -> Result<(), MasterError> {
        self.events.push_back(MasterEvent::ReplacementCompleted {
            active: self.active_id()?.clone(),
        });
        Ok(())
    }

    pub(super) fn fail_master(&mut self, phase: FailurePhase) -> Result<(), MasterError> {
        if matches!(self.stage, Stage::Failed) {
            return Ok(());
        }
        if !matches!(self.stage, Stage::Failing { .. }) {
            self.stage = Stage::Failing { phase };
            self.events.push_back(MasterEvent::FailClosed { phase });
        }
        if self.deferred_active_exit.take().is_some() {
            self.take_worker(WorkerRole::Active);
        }
        for role in [
            WorkerRole::Active,
            WorkerRole::Candidate,
            WorkerRole::Retired,
        ] {
            if self.worker(role).is_some() {
                self.force_role(role)?;
            }
        }
        Ok(())
    }

    pub(super) fn mark_terminating(
        &mut self,
        role: WorkerRole,
        forced: bool,
    ) -> Result<(), MasterError> {
        let worker = self
            .worker_mut(role)
            .ok_or(MasterError::MissingWorker(role))?;
        worker.state = WorkerState::Terminating { forced };
        self.events
            .push_back(MasterEvent::TerminationRequested { role, forced });
        Ok(())
    }

    pub(super) fn force_role(&mut self, role: WorkerRole) -> Result<(), MasterError> {
        let already_forced = matches!(
            self.worker(role).map(|worker| worker.state),
            Some(WorkerState::Terminating { forced: true })
        );
        if already_forced {
            return Ok(());
        }
        let worker = self
            .worker_mut(role)
            .ok_or(MasterError::MissingWorker(role))?;
        worker.process.request_kill()?;
        worker.pending = None;
        worker.state = WorkerState::Terminating { forced: true };
        self.events
            .push_back(MasterEvent::TerminationRequested { role, forced: true });
        Ok(())
    }

    pub(super) fn handle_timeout(&mut self, now: Instant) -> Result<(), MasterError> {
        let Some(next) = self.next_deadline() else {
            return Ok(());
        };
        if now < next {
            return Ok(());
        }
        match self.stage {
            Stage::ShuttingDown { deadline, forced } if now >= deadline => {
                if !forced {
                    self.events.push_back(MasterEvent::ShutdownForced);
                }
                self.stage = Stage::ShuttingDown {
                    deadline,
                    forced: true,
                };
                for role in [
                    WorkerRole::Active,
                    WorkerRole::Candidate,
                    WorkerRole::Retired,
                ] {
                    if self.worker(role).is_some() {
                        self.force_role(role)?;
                    }
                }
                Ok(())
            }
            _ => {
                if self.replacement_phase() == Some(ReplacementPhase::StoppingRetired) {
                    let retired_deadline = self
                        .retired
                        .as_ref()
                        .and_then(|worker| worker.termination_deadline);
                    if retired_deadline.is_some_and(|deadline| deadline <= now) {
                        let retired = self.retired_id()?.clone();
                        self.replacement_failure = Some(FailurePhase::Shutdown);
                        self.worker_mut(WorkerRole::Retired)
                            .ok_or(MasterError::MissingWorker(WorkerRole::Retired))?
                            .termination_deadline = None;
                        return self.apply_replacement_event(
                            ReplacementEvent::TerminationTimedOut {
                                instance_id: retired,
                            },
                            now,
                        );
                    }
                }
                let timed_out = [
                    WorkerRole::Active,
                    WorkerRole::Candidate,
                    WorkerRole::Retired,
                ]
                .into_iter()
                .find(|role| {
                    self.worker(*role)
                        .and_then(|worker| worker.pending)
                        .is_some_and(|pending| pending.deadline <= now)
                });
                if let Some(role) = timed_out {
                    let phase = self
                        .worker(role)
                        .and_then(|worker| worker.pending)
                        .map_or(FailurePhase::Protocol, |pending| {
                            phase_failure(pending.phase)
                        });
                    self.worker_mut(role)
                        .ok_or(MasterError::MissingWorker(role))?
                        .pending = None;
                    self.handle_phase_failure(role, phase, now)?;
                }
                Ok(())
            }
        }
    }

    pub(super) fn finish_terminal_states(&mut self) {
        if self.worker_count() != 0 {
            return;
        }
        match self.stage {
            Stage::ShuttingDown { forced, .. } => {
                self.stage = Stage::Stopped { forced };
                self.events
                    .push_back(MasterEvent::ShutdownCompleted { forced });
            }
            Stage::Failing { phase } => {
                self.stage = Stage::Failed;
                self.events.push_back(MasterEvent::Failed { phase });
            }
            _ => {}
        }
    }
}
