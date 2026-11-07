#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct Pending {
    pub(super) request_id: u64,
    pub(super) phase: ControlPhase,
    pub(super) deadline: Instant,
}

pub(super) struct PreparedRequest {
    pub(super) pending: Pending,
    pub(super) payload: Vec<u8>,
}

pub(super) type SpawnAction<'a> = dyn FnMut(&Instance) -> Result<ManagedWorker, MasterError> + 'a;

#[derive(Debug)]
pub(super) struct ManagedWorker {
    pub(super) instance_id: InstanceId,
    pub(super) generation: GenerationId,
    pub(super) process: WorkerProcess,
    pub(super) pending: Option<Pending>,
    pub(super) termination_deadline: Option<Instant>,
    pub(super) state: WorkerState,
    pub(super) channel_open: bool,
    pub(super) status: Option<WorkerStatus>,
    pub(super) last_event_cursor: u64,
    /// Opaque state the worker last reported with `report_state`.
    pub(super) reported_state: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Stage {
    BootAdopting,
    BootActivating,
    Operational,
    ShuttingDown { deadline: Instant, forced: bool },
    Failing { phase: FailurePhase },
    Stopped { forced: bool },
    Failed,
}

/// Socket-owning deterministic master state machine.
#[derive(Debug)]
pub struct Master<E: ActionExecutor = SystemActionExecutor> {
    pub(super) config: MasterConfig,
    pub(super) listeners: StableListeners,
    /// Listeners the candidate adopts; they replace `listeners` when the candidate commits.
    pub(super) pending_listeners: Option<StableListeners>,
    pub(super) executor: E,
    pub(super) supervisor: Option<ReplacementSupervisor>,
    pub(super) active: Option<ManagedWorker>,
    pub(super) candidate: Option<ManagedWorker>,
    pub(super) retired: Option<ManagedWorker>,
    pub(super) deferred_active_exit: Option<ExitStatus>,
    pub(super) replacement_failure: Option<FailurePhase>,
    pub(super) stage: Stage,
    pub(super) next_request_id: u64,
    pub(super) events: VecDeque<MasterEvent>,
    pub(super) snapshot_history: SupervisorSnapshotHistory,
}

impl Master<SystemActionExecutor> {
    /// Spawns the initial worker using the production action executor.
    ///
    /// # Errors
    ///
    /// Returns an error for identity, spawn, or local invariant failures.
    pub fn launch<F: WorkerFactory>(
        config: MasterConfig,
        listeners: StableListeners,
        factory: &mut F,
        active: WorkerInput<F::Command>,
        now: Instant,
    ) -> Result<Self, MasterError> {
        Self::launch_with_executor(
            config,
            listeners,
            SystemActionExecutor,
            factory,
            active,
            now,
        )
    }
}

impl<E: ActionExecutor> Master<E> {
    /// Spawns the initial worker with an injectable action executor.
    ///
    /// Post-spawn action failures return a fail-closing master so process ownership is retained and
    /// reaped by [`Self::poll`].
    ///
    /// # Errors
    ///
    /// Returns an error for identity, spawn, deadline, request identity, or lifecycle failures.
    pub fn launch_with_executor<F: WorkerFactory>(
        config: MasterConfig,
        listeners: StableListeners,
        executor: E,
        factory: &mut F,
        active: WorkerInput<F::Command>,
        now: Instant,
    ) -> Result<Self, MasterError> {
        validate_identity(active.identity)?;
        let process = factory
            .spawn(active.command, active.identity)
            .map_err(|error| MasterError::Spawn(error.to_string()))?;
        let mut master = Self {
            config,
            listeners,
            pending_listeners: None,
            executor,
            supervisor: None,
            active: Some(ManagedWorker::new(
                active.instance_id,
                active.identity.generation,
                process,
            )),
            candidate: None,
            retired: None,
            deferred_active_exit: None,
            replacement_failure: None,
            stage: Stage::BootAdopting,
            next_request_id: 1,
            events: VecDeque::new(),
            snapshot_history: SupervisorSnapshotHistory::new(),
        };
        if let Err(error) = master.issue_adoption(WorkerRole::Active, now) {
            master.compensate_issue_error(
                WorkerRole::Active,
                ControlPhase::AdoptListeners,
                FailurePhase::ListenerAdoption,
                now,
                error,
            )?;
        }
        Ok(master)
    }

    /// Starts one replacement while the master is running.
    ///
    /// `Begin` is validated against a cloned supervisor before process spawn. Spawn failure leaves
    /// the committed supervisor unchanged and emits [`MasterEvent::SpawnFailed`].
    ///
    /// # Errors
    ///
    /// Returns an error unless the master is idle and running, or for invalid identity, spawn,
    /// lifecycle, deadline, request identity, or local process failures.
    pub fn replace<F: WorkerFactory>(
        &mut self,
        factory: &mut F,
        candidate: WorkerInput<F::Command>,
        now: Instant,
    ) -> Result<(), MasterError> {
        let result = self.begin_replacement(factory, candidate, None, now);
        self.release_unused_pending_listeners();
        result
    }

    /// Starts one replacement whose candidate adopts `listeners` instead of the current ones.
    ///
    /// The new set may keep, add and drop listeners relative to the current one (see
    /// [`oxiroute_supervision_unix::DescriptorManifest::diff`]). The current set stays in force
    /// until the candidate commits: a failed replacement rolls back to the active worker and
    /// discards `listeners`, so the sockets that set added close with it. At commit the master
    /// adopts `listeners` as its stable set and stops the stream listeners the new set dropped, so
    /// they refuse new connections while their accepted connections drain in the retired worker.
    ///
    /// # Errors
    ///
    /// Same as [`Self::replace`].
    pub fn replace_with_listeners<F: WorkerFactory>(
        &mut self,
        factory: &mut F,
        candidate: WorkerInput<F::Command>,
        listeners: StableListeners,
        now: Instant,
    ) -> Result<(), MasterError> {
        let result = self.begin_replacement(factory, candidate, Some(listeners), now);
        self.release_unused_pending_listeners();
        result
    }

    /// Keeps the pending listener set only while a candidate that may adopt it exists.
    fn release_unused_pending_listeners(&mut self) {
        if self.candidate.is_none() {
            self.pending_listeners = None;
        }
    }

    fn begin_replacement<F: WorkerFactory>(
        &mut self,
        factory: &mut F,
        candidate: WorkerInput<F::Command>,
        listeners: Option<StableListeners>,
        now: Instant,
    ) -> Result<(), MasterError> {
        if self.state() != MasterState::Running {
            return Err(MasterError::InvalidState(self.state()));
        }
        validate_identity(candidate.identity)?;
        self.pending_listeners = listeners;
        let instance = Instance {
            instance_id: candidate.instance_id.clone(),
            generation_id: candidate.identity.generation,
            lifecycle: Lifecycle::Spawned,
        };
        let mut validated = self
            .supervisor
            .as_ref()
            .ok_or(MasterError::MissingSupervisor)?
            .clone();
        let actions = validated.apply(ReplacementEvent::Begin {
            candidate: instance,
        })?;
        let candidate_id = candidate.instance_id.clone();
        let generation = candidate.identity.generation;
        let mut command = Some(candidate.command);
        let mut spawn = |instance: &Instance| {
            let process = factory
                .spawn(
                    command
                        .take()
                        .ok_or(MasterError::UnexpectedReplacementActions)?,
                    candidate.identity,
                )
                .map_err(|error| MasterError::Spawn(error.to_string()))?;
            Ok(ManagedWorker::new(
                instance.instance_id.clone(),
                generation,
                process,
            ))
        };
        let original = self.supervisor.replace(validated);
        if let Err(error) = self.execute_replacement_actions(actions, now, Some(&mut spawn)) {
            self.supervisor = original;
            return Err(error);
        }
        self.apply_replacement_event(
            ReplacementEvent::CandidateSpawned {
                instance_id: candidate_id.clone(),
            },
            now,
        )?;
        self.apply_replacement_event(
            ReplacementEvent::CandidateHandshakeComplete {
                instance_id: candidate_id,
            },
            now,
        )?;
        Ok(())
    }

    /// Advances expired deadlines first, then all immediately available process and channel events.
    ///
    /// A matching acknowledgement observed at or after its deadline is stale because expiry clears
    /// or replaces the pending request before observations are collected. At most one frame per
    /// worker is consumed per call.
    ///
    /// # Errors
    ///
    /// Returns an error only for local OS operations or violated replacement invariants.
    pub fn poll(&mut self, now: Instant) -> Result<Vec<MasterEvent>, MasterError> {
        self.handle_timeout(now)?;
        if let Stage::Failing { phase } = self.stage {
            self.fail_master(phase)?;
        }
        let prioritize_activation =
            self.replacement_phase() == Some(ReplacementPhase::ActivatingCandidate);
        let candidate_id = self.candidate_id().ok().cloned();
        let mut observed = Vec::with_capacity(3);
        for role in [
            WorkerRole::Active,
            WorkerRole::Candidate,
            WorkerRole::Retired,
        ] {
            if let Some(observation) = self.observe(role)? {
                observed.push(observation);
            }
        }
        if prioritize_activation {
            observed.sort_by_key(|item| {
                let candidate_activation = candidate_id.as_ref() == Some(&item.instance_id)
                    && matches!(
                        item.observation,
                        Observation::Ack(ControlAck {
                            phase: ControlPhase::Activate,
                            outcome: ControlOutcome::Accepted,
                            ..
                        })
                    );
                !candidate_activation
            });
        }
        for item in observed {
            let Some(role) = self.role_for(&item.instance_id) else {
                continue;
            };
            self.handle_observation(role, item.observation, now)?;
        }
        self.finish_terminal_states();
        self.release_unused_pending_listeners();
        Ok(self.events.drain(..).collect())
    }

    /// Sends the active worker an opaque notice that it does not acknowledge.
    ///
    /// # Errors
    ///
    /// Returns an error when no worker is active, the notice is too large, or the send fails.
    pub fn notify_active(&mut self, notice: &[u8]) -> Result<(), MasterError> {
        let request_id = self
            .take_request_id()
            .map_err(|source| MasterError::Preparation {
                step: PreparationStep::RequestId,
                source,
            })?;
        let payload =
            encode_request(request_id, ControlPhase::Notice, notice).map_err(|source| {
                MasterError::Preparation {
                    step: PreparationStep::Encoding,
                    source: PreparationError::Protocol(source),
                }
            })?;
        let worker = self
            .active
            .as_mut()
            .ok_or(MasterError::MissingWorker(WorkerRole::Active))?;
        self.executor
            .send(
                WorkerRole::Active,
                &mut worker.process,
                ControlPhase::Notice,
                &payload,
                &[],
            )
            .map_err(|source| MasterError::Action {
                action: ActionKind::Send(ControlPhase::Notice),
                source,
            })
    }

    /// Begins or advances bounded asynchronous shutdown without waiting for process exit.
    ///
    /// # Errors
    ///
    /// Returns an error for deadline overflow, request identity exhaustion, or signal failures.
    pub fn shutdown(&mut self, now: Instant) -> Result<ShutdownProgress, MasterError> {
        if !matches!(
            self.stage,
            Stage::ShuttingDown { .. } | Stage::Stopped { .. }
        ) {
            let deadline = deadline(now, self.config.shutdown_timeout())?;
            if self.deferred_active_exit.take().is_some() {
                self.take_worker(WorkerRole::Active);
            }
            self.stage = Stage::ShuttingDown {
                deadline,
                forced: false,
            };
            self.supervisor = None;
            self.replacement_failure = None;
            self.events.push_back(MasterEvent::ShutdownStarted);
            for role in [
                WorkerRole::Active,
                WorkerRole::Candidate,
                WorkerRole::Retired,
            ] {
                if self.worker(role).is_some() {
                    match self.issue_at_deadline(role, ControlPhase::Shutdown, deadline) {
                        Ok(()) => self.mark_terminating(role, false)?,
                        Err(error) if error.is_issue_failure() => {
                            self.record_issue_failure(role, ControlPhase::Shutdown, error)?;
                            self.force_role(role)?;
                            if let Stage::ShuttingDown { forced, .. } = &mut self.stage {
                                *forced = true;
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        self.handle_timeout(now)?;
        Ok(self.shutdown_progress())
    }

    /// Returns current bounded-shutdown progress.
    #[must_use]
    pub fn shutdown_progress(&self) -> ShutdownProgress {
        match self.stage {
            Stage::Stopped { forced } => ShutdownProgress::Complete { forced },
            Stage::ShuttingDown { forced, .. } => ShutdownProgress::Pending {
                remaining: self.worker_count(),
                forced,
            },
            _ => ShutdownProgress::Pending {
                remaining: self.worker_count(),
                forced: false,
            },
        }
    }
}

pub(super) const fn generation_role(role: WorkerRole) -> GenerationRole {
    match role {
        WorkerRole::Active => GenerationRole::Active,
        WorkerRole::Candidate => GenerationRole::Candidate,
        WorkerRole::Retired => GenerationRole::Previous,
    }
}

impl ManagedWorker {
    const fn new(
        instance_id: InstanceId,
        generation: GenerationId,
        process: WorkerProcess,
    ) -> Self {
        Self {
            instance_id,
            generation,
            process,
            pending: None,
            termination_deadline: None,
            state: WorkerState::Running,
            channel_open: true,
            status: None,
            last_event_cursor: 0,
            reported_state: Vec::new(),
        }
    }
}

pub(super) fn channel_disconnected(error: &AuthenticatedChannelError) -> bool {
    matches!(
        error,
        AuthenticatedChannelError::ChannelClosed
            | AuthenticatedChannelError::Transport(
                oxiroute_supervision_unix::TransportError::Closed
                    | oxiroute_supervision_unix::TransportError::Io(rustix::io::Errno::CONNRESET)
            )
    )
}

fn validate_identity(identity: WorkerIdentity) -> Result<(), MasterError> {
    if identity.protocol != CONTROL_PROTOCOL_VERSION {
        return Err(MasterError::ProtocolVersion {
            expected: CONTROL_PROTOCOL_VERSION,
            actual: identity.protocol,
        });
    }
    Ok(())
}

fn deadline(now: Instant, timeout: Duration) -> Result<Instant, MasterError> {
    now.checked_add(timeout)
        .ok_or(MasterError::DeadlineOverflow)
}

pub(super) const fn phase_failure(phase: ControlPhase) -> FailurePhase {
    match phase {
        ControlPhase::AdoptListeners => FailurePhase::ListenerAdoption,
        ControlPhase::Quiesce => FailurePhase::Quiesce,
        ControlPhase::Activate => FailurePhase::Activation,
        ControlPhase::Drain => FailurePhase::Drain,
        ControlPhase::Reactivate => FailurePhase::Reactivation,
        ControlPhase::Shutdown => FailurePhase::Shutdown,
        ControlPhase::Notice => FailurePhase::Protocol,
    }
}
