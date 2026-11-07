#[allow(clippy::wildcard_imports)]
use super::*;

impl<E: ActionExecutor> Master<E> {
    pub(super) fn issue_or_compensate(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        now: Instant,
        timeout: Duration,
        failure: FailurePhase,
    ) -> Result<(), MasterError> {
        match self.issue(role, phase, now, timeout) {
            Ok(_) => Ok(()),
            Err(error) => self.compensate_issue_error(role, phase, failure, now, error),
        }
    }

    pub(super) fn compensate_issue_error(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        failure: FailurePhase,
        now: Instant,
        error: MasterError,
    ) -> Result<(), MasterError> {
        self.record_issue_failure(role, phase, error)?;
        self.handle_phase_failure(role, failure, now)
    }

    pub(super) fn record_issue_failure(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        error: MasterError,
    ) -> Result<(), MasterError> {
        match error {
            MasterError::Action { action, .. } => self
                .events
                .push_back(MasterEvent::ActionFailed { role, action }),
            MasterError::Preparation { step, .. } => {
                self.events
                    .push_back(MasterEvent::PreparationFailed { role, phase, step });
            }
            error => return Err(error),
        }
        Ok(())
    }

    pub(super) fn apply_replacement_event(
        &mut self,
        event: ReplacementEvent,
        now: Instant,
    ) -> Result<(), MasterError> {
        let actions = self.supervisor_mut()?.apply(event)?;
        self.execute_replacement_actions(actions, now, None)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn execute_replacement_actions(
        &mut self,
        actions: Vec<ReplacementAction>,
        now: Instant,
        mut spawn: Option<&mut SpawnAction<'_>>,
    ) -> Result<(), MasterError> {
        let mut actions = VecDeque::from(actions);
        while let Some(action) = actions.pop_front() {
            match action {
                ReplacementAction::Spawn { instance } => {
                    let result = spawn
                        .as_deref_mut()
                        .ok_or(MasterError::UnexpectedReplacementActions)?(
                        &instance
                    );
                    let worker = match result {
                        Ok(worker) => worker,
                        Err(error) => {
                            self.events.push_back(MasterEvent::SpawnFailed {
                                instance_id: instance.instance_id,
                            });
                            return Err(error);
                        }
                    };
                    self.candidate = Some(worker);
                    self.events.push_back(MasterEvent::ReplacementStarted {
                        instance_id: instance.instance_id,
                    });
                }
                ReplacementAction::Prepare { instance_id } => {
                    let role = self
                        .role_for(&instance_id)
                        .ok_or(MasterError::MissingWorker(WorkerRole::Candidate))?;
                    if role != WorkerRole::Candidate {
                        return Err(MasterError::UnexpectedReplacementActions);
                    }
                    if let Err(error) = self.issue_adoption(role, now) {
                        self.compensate_issue_error(
                            role,
                            ControlPhase::AdoptListeners,
                            FailurePhase::ListenerAdoption,
                            now,
                            error,
                        )?;
                    }
                }
                ReplacementAction::Activate { instance_id } => {
                    let role = self
                        .role_for(&instance_id)
                        .ok_or(MasterError::UnexpectedReplacementActions)?;
                    let (phase, failure) = match role {
                        WorkerRole::Active => {
                            (ControlPhase::Reactivate, FailurePhase::Reactivation)
                        }
                        WorkerRole::Candidate => (ControlPhase::Activate, FailurePhase::Activation),
                        WorkerRole::Retired => {
                            return Err(MasterError::UnexpectedReplacementActions);
                        }
                    };
                    self.issue_or_compensate(
                        role,
                        phase,
                        now,
                        self.config.activation_timeout(),
                        failure,
                    )?;
                }
                ReplacementAction::Quiesce { instance_id } => {
                    if self.role_for(&instance_id) != Some(WorkerRole::Active) {
                        return Err(MasterError::UnexpectedReplacementActions);
                    }
                    self.issue_or_compensate(
                        WorkerRole::Active,
                        ControlPhase::Quiesce,
                        now,
                        self.config.quiesce_timeout(),
                        FailurePhase::Quiesce,
                    )?;
                }
                ReplacementAction::Drain { instance_id } => {
                    self.commit_candidate(&instance_id)?;
                    if self.deferred_active_exit.take().is_some() {
                        self.fail_retired(FailurePhase::Crash, now)?;
                        let retired = self.retired_id()?.clone();
                        self.take_worker(WorkerRole::Retired);
                        self.apply_replacement_event(
                            ReplacementEvent::RetiredStopped {
                                instance_id: retired,
                            },
                            now,
                        )?;
                        self.complete_failed_retired()?;
                    } else {
                        self.issue_or_compensate(
                            WorkerRole::Retired,
                            ControlPhase::Drain,
                            now,
                            self.config.drain_timeout(),
                            FailurePhase::Drain,
                        )?;
                    }
                }
                ReplacementAction::Snapshot { instance_id } => {
                    actions.extend(
                        self.supervisor_mut()?
                            .apply(ReplacementEvent::RetiredSnapshotCaptured { instance_id })?,
                    );
                }
                ReplacementAction::Terminate { instance_id } => {
                    let role = self
                        .role_for(&instance_id)
                        .ok_or(MasterError::UnexpectedReplacementActions)?;
                    match role {
                        WorkerRole::Candidate => self.force_role(role)?,
                        WorkerRole::Retired => match self.issue(
                            role,
                            ControlPhase::Shutdown,
                            now,
                            self.config.shutdown_timeout(),
                        ) {
                            Ok(action_deadline) => {
                                self.mark_terminating(role, false)?;
                                self.worker_mut(role)
                                    .ok_or(MasterError::MissingWorker(role))?
                                    .termination_deadline = Some(action_deadline);
                            }
                            Err(error) if error.is_issue_failure() => {
                                self.record_issue_failure(role, ControlPhase::Shutdown, error)?;
                                self.fail_retired(FailurePhase::Shutdown, now)?;
                            }
                            Err(error) => return Err(error),
                        },
                        WorkerRole::Active => {
                            return Err(MasterError::UnexpectedReplacementActions);
                        }
                    }
                }
                ReplacementAction::Kill { instance_id } => {
                    let role = self
                        .role_for(&instance_id)
                        .ok_or(MasterError::UnexpectedReplacementActions)?;
                    self.force_role(role)?;
                }
            }
        }
        Ok(())
    }

    fn commit_candidate(&mut self, retired_id: &InstanceId) -> Result<(), MasterError> {
        let candidate = self
            .supervisor
            .as_ref()
            .ok_or(MasterError::MissingSupervisor)?
            .active()
            .instance_id
            .clone();
        if self.active_id()? != retired_id || self.candidate_id()? != &candidate {
            return Err(MasterError::UnexpectedReplacementActions);
        }
        let old = self
            .active
            .take()
            .ok_or(MasterError::MissingWorker(WorkerRole::Active))?;
        let promoted = self
            .candidate
            .take()
            .ok_or(MasterError::MissingWorker(WorkerRole::Candidate))?;
        self.active = Some(promoted);
        self.retired = Some(old);
        if let Some(next) = self.pending_listeners.take() {
            self.listeners.stop_listening_unless_kept(next.manifest());
            self.listeners = next;
        }
        self.events.push_back(MasterEvent::ReplacementCommitted {
            active: candidate,
            retired: retired_id.clone(),
        });
        Ok(())
    }
}
