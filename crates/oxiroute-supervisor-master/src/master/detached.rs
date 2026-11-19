#[allow(clippy::wildcard_imports)]
use super::*;

/// Replaced workers outlive the replacement that retired them: each one drains on its own until its
/// connections end, its drain deadline passes or the master closes it to stay within the limit.
impl<E: ActionExecutor> Master<E> {
    /// Moves the draining retired worker out of the replacement transaction, so that the next
    /// replacement can start while it keeps serving, then closes the oldest draining workers
    /// the limit leaves no room for.
    pub(super) fn detach_retired(&mut self, now: Instant) -> Result<(), MasterError> {
        if self.replacement_phase() == Some(ReplacementPhase::DrainingRetired) {
            let retired = self.retired_id()?.clone();
            self.apply_replacement_event(
                ReplacementEvent::RetiredDetached {
                    instance_id: retired,
                },
                now,
            )?;
            let worker = self
                .retired
                .take()
                .ok_or(MasterError::MissingWorker(WorkerRole::Retired))?;
            self.detached.push(worker);
        }
        self.enforce_retired_limit(now)
    }

    fn enforce_retired_limit(&mut self, now: Instant) -> Result<(), MasterError> {
        let room = self.retired_limit.saturating_sub(1);
        while self
            .detached
            .iter()
            .filter(|worker| worker.forced.is_none())
            .count()
            > room
        {
            let Some(index) = self
                .detached
                .iter()
                .position(|worker| worker.forced.is_none())
            else {
                break;
            };
            let deadline = runtime::deadline(now, self.config.shutdown_timeout())?;
            self.close_detached(index, Some(RetiredForce::RetiredLimit), deadline)?;
        }
        Ok(())
    }

    /// Asks the detached worker to shut down, and kills it when it does not by the shutdown deadline.
    fn close_detached(
        &mut self,
        index: usize,
        forced: Option<RetiredForce>,
        deadline: Instant,
    ) -> Result<(), MasterError> {
        let request_id = self
            .take_request_id()
            .map_err(|source| MasterError::Preparation {
                step: PreparationStep::RequestId,
                source,
            })?;
        let payload =
            encode_request(request_id, ControlPhase::Shutdown, &[]).map_err(|source| {
                MasterError::Preparation {
                    step: PreparationStep::Encoding,
                    source: PreparationError::Protocol(source),
                }
            })?;
        let worker = &mut self.detached[index];
        let instance_id = worker.instance_id.clone();
        let sent = self.executor.send(
            WorkerRole::Retired,
            &mut worker.process,
            ControlPhase::Shutdown,
            &payload,
            &[],
        );
        worker.pending = None;
        worker.forced = forced.or(worker.forced);
        let terminating_forced = sent.is_err();
        if terminating_forced {
            worker.process.request_kill()?;
            worker.termination_deadline = None;
        } else {
            worker.termination_deadline = Some(deadline);
        }
        worker.state = WorkerState::Terminating {
            forced: terminating_forced,
        };
        if let Some(reason) = forced {
            self.events.push_back(MasterEvent::RetiredForced {
                instance_id,
                reason,
            });
        }
        self.events.push_back(MasterEvent::TerminationRequested {
            role: WorkerRole::Retired,
            forced: terminating_forced,
        });
        Ok(())
    }

    fn kill_detached(&mut self, index: usize, phase: FailurePhase) -> Result<(), MasterError> {
        let worker = &mut self.detached[index];
        worker.process.request_kill()?;
        worker.pending = None;
        worker.termination_deadline = None;
        worker.state = WorkerState::Terminating { forced: true };
        self.events.push_back(MasterEvent::RetiredFailed { phase });
        Ok(())
    }

    /// Advances every detached worker: its observations, then its deadlines.
    pub(super) fn poll_detached(&mut self, now: Instant) -> Result<(), MasterError> {
        let mut index = 0;
        while index < self.detached.len() {
            if self.advance_detached(index, now)? {
                let worker = self.detached.remove(index);
                self.snapshot_history.reap(worker.status.as_ref());
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    fn advance_detached(&mut self, index: usize, now: Instant) -> Result<bool, MasterError> {
        if let Some(observed) = Self::observe_worker(&mut self.detached[index])? {
            match observed.observation {
                Observation::Exit(status) => {
                    self.events.push_back(MasterEvent::WorkerExited {
                        role: WorkerRole::Retired,
                        instance_id: observed.instance_id,
                        status,
                    });
                    return Ok(true);
                }
                Observation::Ack(ack) => self.detached_ack(index, &ack, now)?,
                Observation::Status(status) => record_status(
                    &mut self.snapshot_history,
                    &mut self.events,
                    &mut self.detached[index],
                    WorkerRole::Retired,
                    &status,
                ),
                Observation::State(_) => {}
                Observation::Disconnected => {
                    if matches!(self.detached[index].state, WorkerState::Running) {
                        self.kill_detached(index, FailurePhase::Disconnected)?;
                    }
                }
                Observation::ProtocolFailure => {
                    self.kill_detached(index, FailurePhase::Protocol)?;
                }
            }
        }
        self.expire_detached(index, now)?;
        Ok(false)
    }

    fn detached_ack(
        &mut self,
        index: usize,
        ack: &ControlAck,
        now: Instant,
    ) -> Result<(), MasterError> {
        let pending = self.detached[index].pending;
        if pending.is_none_or(|pending| {
            pending.request_id != ack.request_id || pending.phase != ack.phase
        }) {
            self.events.push_back(MasterEvent::StaleAcknowledgement {
                role: WorkerRole::Retired,
                request_id: ack.request_id,
                phase: ack.phase,
            });
            return Ok(());
        }
        self.detached[index].pending = None;
        match ack.outcome {
            ControlOutcome::Accepted => {
                let deadline = runtime::deadline(now, self.config.shutdown_timeout())?;
                self.close_detached(index, None, deadline)
            }
            ControlOutcome::Rejected(_) => self.kill_detached(index, phase_failure(ack.phase)),
        }
    }

    fn expire_detached(&mut self, index: usize, now: Instant) -> Result<(), MasterError> {
        let worker = &self.detached[index];
        let pending = worker.pending.filter(|pending| pending.deadline <= now);
        let terminating = worker
            .termination_deadline
            .is_some_and(|deadline| deadline <= now);
        if let Some(pending) = pending {
            self.kill_detached(index, phase_failure(pending.phase))?;
        } else if terminating {
            self.kill_detached(index, FailurePhase::Shutdown)?;
        }
        Ok(())
    }

    /// Shuts every detached worker down for a master shutdown, killing the ones that cannot be asked.
    pub(super) fn shutdown_detached(&mut self, deadline: Instant) -> Result<(), MasterError> {
        for index in 0..self.detached.len() {
            self.close_detached(index, None, deadline)?;
        }
        Ok(())
    }

    pub(super) fn kill_every_detached(&mut self) -> Result<(), MasterError> {
        for worker in &mut self.detached {
            if !matches!(worker.state, WorkerState::Terminating { forced: true }) {
                worker.process.request_kill()?;
                worker.pending = None;
                worker.state = WorkerState::Terminating { forced: true };
            }
        }
        Ok(())
    }

    /// Returns the replaced workers that still drain, oldest first.
    #[must_use]
    pub fn retired_workers(&self) -> Vec<RetiredWorker<'_>> {
        self.detached
            .iter()
            .chain(self.retired.as_ref())
            .filter_map(|worker| {
                let (since, window) = worker.retired?;
                Some(RetiredWorker {
                    instance_id: &worker.instance_id,
                    generation: worker.generation,
                    status: worker.status.as_ref(),
                    since,
                    window,
                    forced: worker.forced,
                })
            })
            .collect()
    }
}

/// Retains a newer worker status and its events, and reports a stale one.
pub(super) fn record_status(
    history: &mut SupervisorSnapshotHistory,
    events: &mut VecDeque<MasterEvent>,
    worker: &mut ManagedWorker,
    role: WorkerRole,
    status: &WorkerStatus,
) {
    let previous_sequence = worker.status.as_ref().map_or(0, |status| status.sequence);
    if status.sequence <= previous_sequence {
        events.push_back(MasterEvent::StaleStatus {
            role,
            sequence: status.sequence,
        });
        return;
    }
    let event_cursor = history.record_events(
        generation_role(role),
        &worker.instance_id,
        worker.generation,
        worker.last_event_cursor,
        status,
    );
    let mut retained = status.clone();
    if let Some(previous) = worker.status.as_ref() {
        retain_monotonic_status(previous, &mut retained);
    }
    worker.last_event_cursor = event_cursor;
    worker.status = Some(retained);
    events.push_back(MasterEvent::WorkerStatusUpdated {
        role,
        instance_id: worker.instance_id.clone(),
        sequence: status.sequence,
    });
}
