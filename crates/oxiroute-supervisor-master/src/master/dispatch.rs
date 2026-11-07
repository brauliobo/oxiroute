#[allow(clippy::wildcard_imports)]
use super::*;

impl<E: ActionExecutor> Master<E> {
    /// The candidate adopts the pending set when a replacement brings one; every other worker
    /// adopts the current set.
    const fn adoption_listeners<'a>(
        role: WorkerRole,
        current: &'a StableListeners,
        pending: Option<&'a StableListeners>,
    ) -> &'a StableListeners {
        match (role, pending) {
            (WorkerRole::Candidate, Some(pending)) => pending,
            _ => current,
        }
    }

    pub(super) fn issue_adoption(
        &mut self,
        role: WorkerRole,
        now: Instant,
    ) -> Result<(), MasterError> {
        let request = self.prepare_request(
            role,
            ControlPhase::AdoptListeners,
            now,
            self.config.adoption_timeout(),
            true,
        )?;
        let listeners =
            Self::adoption_listeners(role, &self.listeners, self.pending_listeners.as_ref());
        let duplicates = self
            .executor
            .duplicate_listeners(role, listeners)
            .map_err(|source| MasterError::Action {
                action: ActionKind::DuplicateListeners,
                source,
            })?;
        self.dispatch(role, &request, &duplicates)
    }

    pub(super) fn issue(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        now: Instant,
        timeout: Duration,
    ) -> Result<Instant, MasterError> {
        let request = self.prepare_request(role, phase, now, timeout, false)?;
        let action_deadline = request.pending.deadline;
        self.dispatch(role, &request, &[])?;
        Ok(action_deadline)
    }

    pub(super) fn issue_at_deadline(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        action_deadline: Instant,
    ) -> Result<(), MasterError> {
        self.prepare_step(role, phase, PreparationStep::Deadline)?;
        let request = self.prepare_request_at_deadline(role, phase, action_deadline, false)?;
        self.dispatch(role, &request, &[])
    }

    fn prepare_request(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        now: Instant,
        timeout: Duration,
        adoption: bool,
    ) -> Result<PreparedRequest, MasterError> {
        self.prepare_step(role, phase, PreparationStep::Deadline)?;
        let action_deadline = now.checked_add(timeout).ok_or(MasterError::Preparation {
            step: PreparationStep::Deadline,
            source: PreparationError::DeadlineOverflow,
        })?;
        self.prepare_request_at_deadline(role, phase, action_deadline, adoption)
    }

    fn prepare_request_at_deadline(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        action_deadline: Instant,
        adoption: bool,
    ) -> Result<PreparedRequest, MasterError> {
        self.prepare_step(role, phase, PreparationStep::RequestId)?;
        let request_id = self
            .take_request_id()
            .map_err(|source| MasterError::Preparation {
                step: PreparationStep::RequestId,
                source,
            })?;
        self.prepare_step(role, phase, PreparationStep::Encoding)?;
        self.prepare_step(role, phase, PreparationStep::Allocation)?;
        let payload = if adoption {
            let listeners =
                Self::adoption_listeners(role, &self.listeners, self.pending_listeners.as_ref());
            encode_adopt_request(request_id, listeners.manifest()).map_err(|source| {
                let step = if matches!(source, ControlProtocolError::Allocation) {
                    PreparationStep::Allocation
                } else {
                    PreparationStep::Encoding
                };
                MasterError::Preparation {
                    step,
                    source: PreparationError::Protocol(source),
                }
            })?
        } else {
            let attachment = self.attachment(role, phase);
            encode_request(request_id, phase, attachment).map_err(|source| {
                MasterError::Preparation {
                    step: PreparationStep::Encoding,
                    source: PreparationError::Protocol(source),
                }
            })?
        };
        Ok(PreparedRequest {
            pending: Pending {
                request_id,
                phase,
                deadline: action_deadline,
            },
            payload,
        })
    }

    /// The candidate starts from the state its predecessor reported when it quiesced.
    fn attachment(&self, role: WorkerRole, phase: ControlPhase) -> &[u8] {
        match (role, phase) {
            (WorkerRole::Candidate, ControlPhase::Activate) => self
                .active
                .as_ref()
                .map_or(&[], |active| active.reported_state.as_slice()),
            _ => &[],
        }
    }

    fn prepare_step(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        step: PreparationStep,
    ) -> Result<(), MasterError> {
        self.executor
            .prepare(role, phase, step)
            .map_err(|source| MasterError::Preparation { step, source })
    }

    fn dispatch(
        &mut self,
        role: WorkerRole,
        request: &PreparedRequest,
        descriptors: &[OwnedFd],
    ) -> Result<(), MasterError> {
        let worker = match role {
            WorkerRole::Active => self.active.as_mut(),
            WorkerRole::Candidate => self.candidate.as_mut(),
            WorkerRole::Retired => self.retired.as_mut(),
        }
        .ok_or(MasterError::MissingWorker(role))?;
        self.executor
            .send(
                role,
                &mut worker.process,
                request.pending.phase,
                &request.payload,
                descriptors,
            )
            .map_err(|source| MasterError::Action {
                action: ActionKind::Send(request.pending.phase),
                source,
            })?;
        worker.pending = Some(request.pending);
        Ok(())
    }

    pub(super) fn observe(&mut self, role: WorkerRole) -> Result<Option<Observed>, MasterError> {
        if role == WorkerRole::Active
            && self.deferred_active_exit.is_some()
            && self.replacement_phase() == Some(ReplacementPhase::ActivatingCandidate)
        {
            return Ok(None);
        }
        let Some(worker) = self.worker_mut(role) else {
            return Ok(None);
        };
        let instance_id = worker.instance_id.clone();
        if let Some(WorkerEvent::ProcessGroupExited(status)) = worker.process.poll_event()? {
            return Ok(Some(Observed {
                instance_id,
                observation: Observation::Exit(status),
            }));
        }
        if !worker.channel_open {
            return Ok(None);
        }
        let observation = match worker.process.channel().try_receive() {
            Ok(Some(frame)) => match frame.header().message_type() {
                crate::protocol::ACK => match decode_ack(&frame) {
                    Ok(ack) => Some(Observation::Ack(ack)),
                    Err(_) => Some(Observation::ProtocolFailure),
                },
                crate::status::STATUS_MESSAGE => match decode_status(&frame) {
                    Ok(status) => Some(Observation::Status(Box::new(status))),
                    Err(_) => Some(Observation::ProtocolFailure),
                },
                crate::status::STATE_MESSAGE
                    if frame.payload().len() <= MAX_ATTACHMENT_BYTES
                        && frame.descriptors().is_empty() =>
                {
                    Some(Observation::State(frame.payload().to_vec()))
                }
                _ => Some(Observation::ProtocolFailure),
            },
            Ok(None) => None,
            Err(AuthenticatedChannelError::WorkerGroupExited(status)) => {
                Some(Observation::Exit(status))
            }
            Err(error) if channel_disconnected(&error) => {
                worker.channel_open = false;
                Some(Observation::Disconnected)
            }
            Err(_) => Some(Observation::ProtocolFailure),
        };
        Ok(observation.map(|observation| Observed {
            instance_id,
            observation,
        }))
    }

    pub(super) fn handle_observation(
        &mut self,
        role: WorkerRole,
        observation: Observation,
        now: Instant,
    ) -> Result<(), MasterError> {
        match observation {
            Observation::Ack(ack) => self.handle_ack(role, ack, now),
            Observation::Status(status) => self.handle_status(role, &status),
            Observation::State(state) => {
                self.worker_mut(role)
                    .ok_or(MasterError::MissingWorker(role))?
                    .reported_state = state;
                Ok(())
            }
            Observation::Exit(status) => self.handle_exit(role, status, now),
            Observation::Disconnected => self.handle_disconnect(role, now),
            Observation::ProtocolFailure => {
                self.handle_phase_failure(role, FailurePhase::Protocol, now)
            }
        }
    }

    fn handle_status(
        &mut self,
        role: WorkerRole,
        status: &WorkerStatus,
    ) -> Result<(), MasterError> {
        let (instance_id, generation, previous_sequence, previous_event_cursor) = {
            let worker = self.worker(role).ok_or(MasterError::MissingWorker(role))?;
            (
                worker.instance_id.clone(),
                worker.generation,
                worker.status.as_ref().map_or(0, |status| status.sequence),
                worker.last_event_cursor,
            )
        };
        if status.sequence <= previous_sequence {
            self.events.push_back(MasterEvent::StaleStatus {
                role,
                sequence: status.sequence,
            });
            return Ok(());
        }
        let event_cursor = self.snapshot_history.record_events(
            generation_role(role),
            &instance_id,
            generation,
            previous_event_cursor,
            status,
        );
        let mut retained = status.clone();
        if let Some(previous) = self.worker(role).and_then(|worker| worker.status.as_ref()) {
            retain_monotonic_status(previous, &mut retained);
        }
        let worker = self
            .worker_mut(role)
            .ok_or(MasterError::MissingWorker(role))?;
        worker.last_event_cursor = event_cursor;
        worker.status = Some(retained);
        self.events.push_back(MasterEvent::WorkerStatusUpdated {
            role,
            instance_id,
            sequence: status.sequence,
        });
        Ok(())
    }

    fn handle_ack(
        &mut self,
        role: WorkerRole,
        ack: ControlAck,
        now: Instant,
    ) -> Result<(), MasterError> {
        let pending = self.worker(role).and_then(|worker| worker.pending);
        if pending.is_none_or(|pending| {
            pending.request_id != ack.request_id || pending.phase != ack.phase
        }) {
            self.events.push_back(MasterEvent::StaleAcknowledgement {
                role,
                request_id: ack.request_id,
                phase: ack.phase,
            });
            return Ok(());
        }
        self.worker_mut(role)
            .ok_or(MasterError::MissingWorker(role))?
            .pending = None;
        if let ControlOutcome::Rejected(_) = ack.outcome {
            return self.handle_phase_failure(role, phase_failure(ack.phase), now);
        }
        self.handle_phase_success(role, ack.phase, now)
    }

    fn handle_phase_success(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        now: Instant,
    ) -> Result<(), MasterError> {
        match (self.stage, role, phase) {
            (Stage::BootAdopting, WorkerRole::Active, ControlPhase::AdoptListeners) => {
                self.stage = Stage::BootActivating;
                self.issue_or_compensate(
                    WorkerRole::Active,
                    ControlPhase::Activate,
                    now,
                    self.config.activation_timeout(),
                    FailurePhase::Activation,
                )
            }
            (Stage::BootActivating, WorkerRole::Active, ControlPhase::Activate) => {
                let active = self
                    .active
                    .as_ref()
                    .ok_or(MasterError::MissingWorker(WorkerRole::Active))?;
                self.supervisor = Some(ReplacementSupervisor::new(Instance {
                    instance_id: active.instance_id.clone(),
                    generation_id: active.generation,
                    lifecycle: Lifecycle::Active,
                })?);
                self.stage = Stage::Operational;
                self.events.push_back(MasterEvent::InitialActivated {
                    instance_id: active.instance_id.clone(),
                });
                Ok(())
            }
            (Stage::Operational, WorkerRole::Candidate, ControlPhase::AdoptListeners)
                if self.replacement_phase() == Some(ReplacementPhase::AdoptingCandidate) =>
            {
                self.apply_replacement_event(
                    ReplacementEvent::CandidatePrepared {
                        instance_id: self.candidate_id()?.clone(),
                    },
                    now,
                )
            }
            (Stage::Operational, WorkerRole::Active, ControlPhase::Quiesce)
                if self.replacement_phase() == Some(ReplacementPhase::Quiescing) =>
            {
                self.apply_replacement_event(
                    ReplacementEvent::ActiveQuiesced {
                        instance_id: self.active_id()?.clone(),
                    },
                    now,
                )
            }
            (Stage::Operational, WorkerRole::Candidate, ControlPhase::Activate)
                if self.replacement_phase() == Some(ReplacementPhase::ActivatingCandidate) =>
            {
                self.apply_replacement_event(
                    ReplacementEvent::CandidateActivated {
                        instance_id: self.candidate_id()?.clone(),
                    },
                    now,
                )
            }
            (Stage::Operational, WorkerRole::Active, ControlPhase::Reactivate)
                if self.replacement_phase() == Some(ReplacementPhase::RollingBack) =>
            {
                self.apply_replacement_event(
                    ReplacementEvent::ActiveReactivated {
                        instance_id: self.active_id()?.clone(),
                    },
                    now,
                )?;
                self.maybe_finish_rollback()
            }
            (Stage::Operational, WorkerRole::Retired, ControlPhase::Drain)
                if self.replacement_phase() == Some(ReplacementPhase::DrainingRetired) =>
            {
                self.apply_replacement_event(
                    ReplacementEvent::RetiredDrained {
                        instance_id: self.retired_id()?.clone(),
                    },
                    now,
                )
            }
            (Stage::Operational, WorkerRole::Retired, ControlPhase::Shutdown)
                if self.replacement_phase() == Some(ReplacementPhase::StoppingRetired) =>
            {
                Ok(())
            }
            (Stage::ShuttingDown { .. }, _, ControlPhase::Shutdown) => Ok(()),
            _ => Err(MasterError::UnexpectedAcknowledgement),
        }
    }
}
