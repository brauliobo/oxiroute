use std::{
    collections::VecDeque,
    error::Error,
    fmt, io,
    os::fd::{AsFd as _, OwnedFd},
    process::ExitStatus,
    time::{Duration, Instant},
};

use oxiroute_supervision::{
    GenerationId, Instance, InstanceId, Lifecycle, ReplacementAction, ReplacementError,
    ReplacementEvent, ReplacementPhase, ReplacementSupervisor,
};
use oxiroute_supervision_unix::FrameFlags;
use oxiroute_supervisor_process::{
    AuthenticatedChannelError, SpawnError, WorkerCommand, WorkerEvent, WorkerIdentity,
    WorkerProcess, WorkerSpawner,
};
use thiserror::Error;

use crate::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, ControlProtocolError, MasterConfig,
    StableListeners,
    listeners::ListenerOwnershipError,
    protocol::{ControlAck, decode_ack, encode_adopt_request, encode_request},
    status::{AggregatedWorkerEvent, MAX_AGGREGATED_EVENTS, WorkerStatus, decode_status},
};

mod observation;

use observation::{Observation, Observed};

/// Factory boundary for worker command inputs. Production callers can use [`WorkerSpawner`].
pub trait WorkerFactory {
    /// Caller-defined command input consumed for one spawn.
    type Command;
    /// Spawn failure returned before the worker enters master state.
    type Error: Error;

    /// Spawns and authenticates one worker process.
    ///
    /// # Errors
    ///
    /// Returns the factory-specific spawn failure.
    fn spawn(
        &mut self,
        command: Self::Command,
        identity: WorkerIdentity,
    ) -> Result<WorkerProcess, Self::Error>;
}

impl WorkerFactory for WorkerSpawner {
    type Command = WorkerCommand;
    type Error = SpawnError;

    fn spawn(
        &mut self,
        command: Self::Command,
        identity: WorkerIdentity,
    ) -> Result<WorkerProcess, Self::Error> {
        WorkerSpawner::spawn(self, command, identity)
    }
}

/// One worker command and its logical and authenticated process identities.
#[derive(Debug)]
pub struct WorkerInput<C> {
    /// Logical instance identity used by the replacement supervisor.
    pub instance_id: InstanceId,
    /// Authenticated process identity. Its generation is also the logical generation.
    pub identity: WorkerIdentity,
    /// Factory-specific command input.
    pub command: C,
}

/// Stable role assigned to an observed worker at the time of an event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerRole {
    /// Current serving worker.
    Active,
    /// Worker being prepared before commit.
    Candidate,
    /// Old worker draining after commit.
    Retired,
}

/// Per-role process ownership state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerState {
    /// Worker remains eligible for protocol actions.
    Running,
    /// Termination was requested and ownership is retained until a process event reaps it.
    Terminating { forced: bool },
}

/// Public state summary for event-loop integration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MasterState {
    /// Initial worker is adopting listeners.
    Starting,
    /// Initial worker is being activated.
    ActivatingInitial,
    /// One active worker is serving and no replacement is in progress.
    Running,
    /// Candidate is adopting listeners.
    AdoptingCandidate,
    /// Active worker is quiescing.
    Quiescing,
    /// Candidate is being activated before commit.
    ActivatingCandidate,
    /// Old active worker is being reactivated while the candidate is reaped.
    RollingBack,
    /// Committed old worker is draining.
    DrainingRetired,
    /// Retired worker remains owned until it is reaped.
    StoppingRetired,
    /// All workers are being shut down.
    ShuttingDown,
    /// Serving failed closed and remaining workers are being reaped.
    Failing,
    /// All workers have been reaped after shutdown.
    Stopped,
    /// Fail-closed process termination completed.
    Failed,
}

/// Phase attributed to a rejection, timeout, protocol failure, or crash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailurePhase {
    /// Listener adoption.
    ListenerAdoption,
    /// Initial or candidate activation.
    Activation,
    /// Active quiescence.
    Quiesce,
    /// Rollback reactivation.
    Reactivation,
    /// Retired-worker drain.
    Drain,
    /// Worker shutdown.
    Shutdown,
    /// Worker process exited outside an expected stopping state.
    Crash,
    /// Worker channel disconnected before process exit was observable.
    Disconnected,
    /// Authenticated message did not satisfy the typed control protocol.
    Protocol,
}

/// Externally visible action attempted by the master.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    /// Duplicate the stable listener set before descriptor transfer.
    DuplicateListeners,
    /// Send one typed phase request.
    Send(ControlPhase),
}

/// Fallible request preparation step exposed for deterministic fault injection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationStep {
    /// Reserve the next monotonic request identity.
    RequestId,
    /// Compute the absolute phase deadline.
    Deadline,
    /// Encode the typed request payload.
    Encoding,
    /// Reserve bounded payload storage.
    Allocation,
}

/// Request preparation failure.
#[derive(Debug, Error)]
pub enum PreparationError {
    /// Deterministic test executor failure.
    #[error("injected request preparation failure")]
    Injected,
    /// Monotonic request identity overflowed.
    #[error("master request identity is exhausted")]
    RequestIdExhausted,
    /// Monotonic deadline could not be represented.
    #[error("master deadline overflowed")]
    DeadlineOverflow,
    /// Typed request encoding failed.
    #[error(transparent)]
    Protocol(#[from] ControlProtocolError),
    /// Bounded request payload allocation failed.
    #[error("bounded request payload allocation failed")]
    Allocation,
}

/// Action execution failure. The injected variant supports deterministic conformance tests.
#[derive(Debug, Error)]
pub enum ActionError {
    /// Listener duplication failed.
    #[error(transparent)]
    Listeners(#[from] ListenerOwnershipError),
    /// Authenticated channel send failed.
    #[error(transparent)]
    Channel(#[from] AuthenticatedChannelError),
    /// Deterministic test executor failure.
    #[error("injected master action failure")]
    Injected,
}

/// Injectable boundary for descriptor duplication and authenticated sends.
pub trait ActionExecutor: fmt::Debug {
    /// Fault-injection hook called before each fallible preparation step.
    ///
    /// Production executors use the default no-op implementation.
    ///
    /// # Errors
    ///
    /// Returns an injected preparation failure before the step executes.
    fn prepare(
        &mut self,
        _role: WorkerRole,
        _phase: ControlPhase,
        _step: PreparationStep,
    ) -> Result<(), PreparationError> {
        Ok(())
    }

    /// Creates temporary listener duplicates for one role.
    ///
    /// # Errors
    ///
    /// Returns an action-specific failure without changing master state.
    fn duplicate_listeners(
        &mut self,
        role: WorkerRole,
        listeners: &StableListeners,
    ) -> Result<Vec<OwnedFd>, ActionError>;

    /// Sends one authenticated request without waiting for its acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns an action-specific failure. Callers compensate based on the current committed state.
    fn send(
        &mut self,
        role: WorkerRole,
        process: &mut WorkerProcess,
        phase: ControlPhase,
        payload: &[u8],
        descriptors: &[OwnedFd],
    ) -> Result<(), ActionError>;
}

/// Production action executor using `CLOEXEC` duplicates and the authenticated worker channel.
#[derive(Debug, Default)]
pub struct SystemActionExecutor;

impl ActionExecutor for SystemActionExecutor {
    fn duplicate_listeners(
        &mut self,
        _role: WorkerRole,
        listeners: &StableListeners,
    ) -> Result<Vec<OwnedFd>, ActionError> {
        Ok(listeners.duplicates()?)
    }

    fn send(
        &mut self,
        _role: WorkerRole,
        process: &mut WorkerProcess,
        phase: ControlPhase,
        payload: &[u8],
        descriptors: &[OwnedFd],
    ) -> Result<(), ActionError> {
        let borrowed = descriptors.iter().map(OwnedFd::as_fd).collect::<Vec<_>>();
        process.channel().send(
            phase.message_type(),
            FrameFlags::default(),
            payload,
            &borrowed,
        )?;
        Ok(())
    }
}

/// Explicit bounded-shutdown result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownProgress {
    /// Workers remain owned and will be reaped by later [`Master::poll`] calls.
    Pending { remaining: usize, forced: bool },
    /// Every worker process group has been reaped.
    Complete { forced: bool },
}

/// Observable state-machine output returned by [`Master::poll`].
#[derive(Debug)]
pub enum MasterEvent {
    /// Initial worker adopted and activated the master-owned listeners.
    InitialActivated { instance_id: InstanceId },
    /// Candidate validation passed but process spawn failed before state mutation.
    SpawnFailed { instance_id: InstanceId },
    /// A candidate entered the replacement transaction.
    ReplacementStarted { instance_id: InstanceId },
    /// Candidate activation acknowledged; role ownership has committed.
    ReplacementCommitted {
        active: InstanceId,
        retired: InstanceId,
    },
    /// Retired worker exited and was reaped.
    ReplacementCompleted { active: InstanceId },
    /// Candidate was discarded and old active recovery began.
    RollbackStarted {
        candidate: InstanceId,
        phase: FailurePhase,
    },
    /// Old active worker was restored and the candidate was reaped.
    RollbackCompleted { active: InstanceId },
    /// One external action failed and compensation was started.
    ActionFailed {
        role: WorkerRole,
        action: ActionKind,
    },
    /// Request preparation failed and phase compensation was started.
    PreparationFailed {
        role: WorkerRole,
        phase: ControlPhase,
        step: PreparationStep,
    },
    /// An authenticated but non-current acknowledgement was ignored.
    StaleAcknowledgement {
        role: WorkerRole,
        request_id: u64,
        phase: ControlPhase,
    },
    /// Channel closure was recorded while process ownership remained pending.
    WorkerDisconnected {
        role: WorkerRole,
        instance_id: InstanceId,
    },
    /// A process-group exit was observed and reaped.
    WorkerExited {
        role: WorkerRole,
        instance_id: InstanceId,
        status: ExitStatus,
    },
    /// A process role entered asynchronous termination.
    TerminationRequested { role: WorkerRole, forced: bool },
    /// A committed retired worker failed; the candidate remains active.
    RetiredFailed { phase: FailurePhase },
    /// Bounded shutdown began.
    ShutdownStarted,
    /// Shutdown deadline forced at least one remaining process group.
    ShutdownForced,
    /// All worker process groups were reaped.
    ShutdownCompleted { forced: bool },
    /// Serving invariants failed and asynchronous fail-closed termination began.
    FailClosed { phase: FailurePhase },
    /// Fail-closed termination completed.
    Failed { phase: FailurePhase },
    /// A newer authenticated worker status observation was retained.
    WorkerStatusUpdated {
        role: WorkerRole,
        instance_id: InstanceId,
        sequence: u64,
    },
    /// A worker status observation arrived after a newer observation was retained.
    StaleStatus { role: WorkerRole, sequence: u64 },
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    request_id: u64,
    phase: ControlPhase,
    deadline: Instant,
}

struct PreparedRequest {
    pending: Pending,
    payload: Vec<u8>,
}

type SpawnAction<'a> = dyn FnMut(&Instance) -> Result<ManagedWorker, MasterError> + 'a;

#[derive(Debug)]
struct ManagedWorker {
    instance_id: InstanceId,
    generation: GenerationId,
    process: WorkerProcess,
    pending: Option<Pending>,
    termination_deadline: Option<Instant>,
    state: WorkerState,
    channel_open: bool,
    status: Option<WorkerStatus>,
    last_event_cursor: u64,
}

#[derive(Clone, Copy, Debug)]
enum Stage {
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
    config: MasterConfig,
    listeners: StableListeners,
    executor: E,
    supervisor: Option<ReplacementSupervisor>,
    active: Option<ManagedWorker>,
    candidate: Option<ManagedWorker>,
    retired: Option<ManagedWorker>,
    deferred_active_exit: Option<ExitStatus>,
    replacement_failure: Option<FailurePhase>,
    stage: Stage,
    next_request_id: u64,
    events: VecDeque<MasterEvent>,
    aggregated_events: VecDeque<AggregatedWorkerEvent>,
    next_aggregated_event_cursor: u64,
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
            aggregated_events: VecDeque::new(),
            next_aggregated_event_cursor: 1,
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
        if self.state() != MasterState::Running {
            return Err(MasterError::InvalidState(self.state()));
        }
        validate_identity(candidate.identity)?;
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
        Ok(self.events.drain(..).collect())
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
                self.active.take();
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

    /// Returns the current public state.
    ///
    /// # Panics
    ///
    /// Panics if the internal operational stage has lost its replacement supervisor.
    #[must_use]
    pub fn state(&self) -> MasterState {
        match self.stage {
            Stage::BootAdopting => MasterState::Starting,
            Stage::BootActivating => MasterState::ActivatingInitial,
            Stage::Operational => match self
                .supervisor
                .as_ref()
                .expect("operational master has a replacement supervisor")
                .phase()
            {
                ReplacementPhase::Running => MasterState::Running,
                ReplacementPhase::AdoptingCandidate => MasterState::AdoptingCandidate,
                ReplacementPhase::Quiescing => MasterState::Quiescing,
                ReplacementPhase::ActivatingCandidate => MasterState::ActivatingCandidate,
                ReplacementPhase::RollingBack => MasterState::RollingBack,
                ReplacementPhase::DrainingRetired => MasterState::DrainingRetired,
                ReplacementPhase::StoppingRetired => MasterState::StoppingRetired,
            },
            Stage::ShuttingDown { .. } => MasterState::ShuttingDown,
            Stage::Failing { .. } => MasterState::Failing,
            Stage::Stopped { .. } => MasterState::Stopped,
            Stage::Failed => MasterState::Failed,
        }
    }

    /// Returns the logical identity currently assigned the active role.
    #[must_use]
    pub fn active_instance(&self) -> Option<&InstanceId> {
        self.active.as_ref().map(|worker| &worker.instance_id)
    }

    /// Returns current process ownership state for `role`.
    #[must_use]
    pub fn worker_state(&self, role: WorkerRole) -> Option<WorkerState> {
        self.worker(role).map(|worker| worker.state)
    }

    /// Returns the authenticated worker PID currently assigned to `role`.
    #[must_use]
    pub fn worker_id(&self, role: WorkerRole) -> Option<u32> {
        self.worker(role).map(|worker| worker.process.id())
    }

    /// Returns the pinned launcher process-group ID currently assigned to `role`.
    #[must_use]
    pub fn worker_process_group_id(&self, role: WorkerRole) -> Option<u32> {
        self.worker(role)
            .and_then(|worker| worker.process.process_group_id())
    }

    /// Returns the latest authenticated status observation retained for `role`.
    #[must_use]
    pub fn worker_status(&self, role: WorkerRole) -> Option<&WorkerStatus> {
        self.worker(role).and_then(|worker| worker.status.as_ref())
    }

    /// Returns bounded generation-qualified worker events newer than `after`.
    #[must_use]
    pub fn worker_events(&self, after: u64, limit: usize) -> Vec<AggregatedWorkerEvent> {
        self.aggregated_events
            .iter()
            .filter(|event| event.cursor > after)
            .take(limit.min(MAX_AGGREGATED_EVENTS))
            .cloned()
            .collect()
    }

    /// Returns the stable listener manifest whose original descriptors remain master-owned.
    #[must_use]
    pub const fn listener_manifest(&self) -> &oxiroute_supervision_unix::DescriptorManifest {
        self.listeners.manifest()
    }

    /// Returns mutable access to the injected action executor.
    pub const fn action_executor_mut(&mut self) -> &mut E {
        &mut self.executor
    }

    /// Returns the earliest state-machine deadline, if any.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        match self.stage {
            Stage::ShuttingDown { deadline, .. } => Some(deadline),
            _ => [
                self.active.as_ref(),
                self.candidate.as_ref(),
                self.retired.as_ref(),
            ]
            .into_iter()
            .flatten()
            .filter_map(|worker| {
                worker
                    .pending
                    .map(|pending| pending.deadline)
                    .into_iter()
                    .chain(worker.termination_deadline)
                    .min()
            })
            .min(),
        }
    }

    fn issue_adoption(&mut self, role: WorkerRole, now: Instant) -> Result<(), MasterError> {
        let request = self.prepare_request(
            role,
            ControlPhase::AdoptListeners,
            now,
            self.config.adoption_timeout(),
            true,
        )?;
        let duplicates = self
            .executor
            .duplicate_listeners(role, &self.listeners)
            .map_err(|source| MasterError::Action {
                action: ActionKind::DuplicateListeners,
                source,
            })?;
        self.dispatch(role, &request, &duplicates)
    }

    fn issue(
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

    fn issue_at_deadline(
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
            encode_adopt_request(request_id, self.listeners.manifest()).map_err(|source| {
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
            let encoded = encode_request(request_id, phase);
            let mut payload = Vec::new();
            payload
                .try_reserve_exact(encoded.len())
                .map_err(|_| MasterError::Preparation {
                    step: PreparationStep::Allocation,
                    source: PreparationError::Allocation,
                })?;
            payload.extend_from_slice(&encoded);
            payload
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

    fn observe(&mut self, role: WorkerRole) -> Result<Option<Observed>, MasterError> {
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

    fn handle_observation(
        &mut self,
        role: WorkerRole,
        observation: Observation,
        now: Instant,
    ) -> Result<(), MasterError> {
        match observation {
            Observation::Ack(ack) => self.handle_ack(role, ack, now),
            Observation::Status(status) => self.handle_status(role, &status),
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
        let mut event_cursor = previous_event_cursor;
        for worker_event in &status.events {
            if worker_event.cursor <= event_cursor {
                continue;
            }
            let cursor = self.next_aggregated_event_cursor;
            self.next_aggregated_event_cursor = cursor.saturating_add(1);
            self.aggregated_events.push_back(AggregatedWorkerEvent {
                cursor,
                instance_id: instance_id.clone(),
                generation_id: generation,
                worker_event: worker_event.clone(),
            });
            while self.aggregated_events.len() > MAX_AGGREGATED_EVENTS {
                self.aggregated_events.pop_front();
            }
            event_cursor = worker_event.cursor;
        }
        event_cursor = event_cursor.max(status.event_cursor);
        let worker = self
            .worker_mut(role)
            .ok_or(MasterError::MissingWorker(role))?;
        worker.last_event_cursor = event_cursor;
        worker.status = Some(status.clone());
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

    fn issue_or_compensate(
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

    fn compensate_issue_error(
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

    fn record_issue_failure(
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

    fn apply_replacement_event(
        &mut self,
        event: ReplacementEvent,
        now: Instant,
    ) -> Result<(), MasterError> {
        let actions = self.supervisor_mut()?.apply(event)?;
        self.execute_replacement_actions(actions, now, None)
    }

    #[allow(clippy::too_many_lines)]
    fn execute_replacement_actions(
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
                        self.retired.take();
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
        self.events.push_back(MasterEvent::ReplacementCommitted {
            active: candidate,
            retired: retired_id.clone(),
        });
        Ok(())
    }

    fn handle_phase_failure(
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

    fn handle_disconnect(&mut self, role: WorkerRole, now: Instant) -> Result<(), MasterError> {
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

    fn handle_exit(
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

    fn maybe_finish_rollback(&mut self) -> Result<(), MasterError> {
        if self.replacement_phase() == Some(ReplacementPhase::Running)
            && self.replacement_failure.take().is_some()
        {
            let active = self.active_id()?.clone();
            self.events
                .push_back(MasterEvent::RollbackCompleted { active });
        }
        Ok(())
    }

    fn fail_retired(&mut self, phase: FailurePhase, now: Instant) -> Result<(), MasterError> {
        let retired = self.retired_id()?.clone();
        self.replacement_failure = Some(phase);
        self.apply_replacement_event(
            ReplacementEvent::RetiredFailed {
                instance_id: retired,
            },
            now,
        )
    }

    fn complete_failed_retired(&mut self) -> Result<(), MasterError> {
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

    fn fail_master(&mut self, phase: FailurePhase) -> Result<(), MasterError> {
        if matches!(self.stage, Stage::Failed) {
            return Ok(());
        }
        if !matches!(self.stage, Stage::Failing { .. }) {
            self.stage = Stage::Failing { phase };
            self.events.push_back(MasterEvent::FailClosed { phase });
        }
        if self.deferred_active_exit.take().is_some() {
            self.active.take();
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

    fn mark_terminating(&mut self, role: WorkerRole, forced: bool) -> Result<(), MasterError> {
        let worker = self
            .worker_mut(role)
            .ok_or(MasterError::MissingWorker(role))?;
        worker.state = WorkerState::Terminating { forced };
        self.events
            .push_back(MasterEvent::TerminationRequested { role, forced });
        Ok(())
    }

    fn force_role(&mut self, role: WorkerRole) -> Result<(), MasterError> {
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

    fn handle_timeout(&mut self, now: Instant) -> Result<(), MasterError> {
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

    fn finish_terminal_states(&mut self) {
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

    fn worker(&self, role: WorkerRole) -> Option<&ManagedWorker> {
        match role {
            WorkerRole::Active => self.active.as_ref(),
            WorkerRole::Candidate => self.candidate.as_ref(),
            WorkerRole::Retired => self.retired.as_ref(),
        }
    }

    fn worker_mut(&mut self, role: WorkerRole) -> Option<&mut ManagedWorker> {
        match role {
            WorkerRole::Active => self.active.as_mut(),
            WorkerRole::Candidate => self.candidate.as_mut(),
            WorkerRole::Retired => self.retired.as_mut(),
        }
    }

    fn take_worker(&mut self, role: WorkerRole) -> Option<ManagedWorker> {
        match role {
            WorkerRole::Active => self.active.take(),
            WorkerRole::Candidate => self.candidate.take(),
            WorkerRole::Retired => self.retired.take(),
        }
    }

    fn role_for(&self, instance_id: &InstanceId) -> Option<WorkerRole> {
        [
            (WorkerRole::Active, self.active.as_ref()),
            (WorkerRole::Candidate, self.candidate.as_ref()),
            (WorkerRole::Retired, self.retired.as_ref()),
        ]
        .into_iter()
        .find_map(|(role, worker)| {
            worker
                .filter(|worker| worker.instance_id == *instance_id)
                .map(|_| role)
        })
    }

    fn worker_count(&self) -> usize {
        [
            self.active.as_ref(),
            self.candidate.as_ref(),
            self.retired.as_ref(),
        ]
        .into_iter()
        .flatten()
        .count()
    }

    fn take_request_id(&mut self) -> Result<u64, PreparationError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .ok_or(PreparationError::RequestIdExhausted)?;
        Ok(request_id)
    }

    fn supervisor_mut(&mut self) -> Result<&mut ReplacementSupervisor, MasterError> {
        self.supervisor
            .as_mut()
            .ok_or(MasterError::MissingSupervisor)
    }

    fn active_id(&self) -> Result<&InstanceId, MasterError> {
        self.active_instance()
            .ok_or(MasterError::MissingWorker(WorkerRole::Active))
    }

    fn candidate_id(&self) -> Result<&InstanceId, MasterError> {
        self.candidate
            .as_ref()
            .map(|worker| &worker.instance_id)
            .ok_or(MasterError::MissingWorker(WorkerRole::Candidate))
    }

    fn retired_id(&self) -> Result<&InstanceId, MasterError> {
        self.retired
            .as_ref()
            .map(|worker| &worker.instance_id)
            .ok_or(MasterError::MissingWorker(WorkerRole::Retired))
    }

    fn replacement_phase(&self) -> Option<ReplacementPhase> {
        self.supervisor.as_ref().map(ReplacementSupervisor::phase)
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
        }
    }
}

fn channel_disconnected(error: &AuthenticatedChannelError) -> bool {
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

const fn phase_failure(phase: ControlPhase) -> FailurePhase {
    match phase {
        ControlPhase::AdoptListeners => FailurePhase::ListenerAdoption,
        ControlPhase::Quiesce => FailurePhase::Quiesce,
        ControlPhase::Activate => FailurePhase::Activation,
        ControlPhase::Drain => FailurePhase::Drain,
        ControlPhase::Reactivate => FailurePhase::Reactivation,
        ControlPhase::Shutdown => FailurePhase::Shutdown,
    }
}

/// Synchronous setup, local I/O, or internal invariant failure.
#[derive(Debug, Error)]
pub enum MasterError {
    /// Worker factory failed before ownership entered the state machine.
    #[error("worker spawn failed: {0}")]
    Spawn(String),
    /// Master operation was not valid in the current state.
    #[error("master operation is invalid in state {0:?}")]
    InvalidState(MasterState),
    /// Worker authenticated a different application protocol version.
    #[error("worker protocol version {actual} does not match {expected}")]
    ProtocolVersion { expected: u16, actual: u16 },
    /// Monotonic deadline could not be represented.
    #[error("master deadline overflowed")]
    DeadlineOverflow,
    /// Required worker role was absent.
    #[error("master has no {0:?} worker")]
    MissingWorker(WorkerRole),
    /// Replacement state was absent outside initial startup or shutdown.
    #[error("master replacement supervisor is absent")]
    MissingSupervisor,
    /// A matched acknowledgement was impossible in the current committed stage.
    #[error("matched acknowledgement is invalid in the current master stage")]
    UnexpectedAcknowledgement,
    /// Pure replacement state emitted an action sequence this adapter does not support.
    #[error("replacement supervisor emitted unexpected actions")]
    UnexpectedReplacementActions,
    /// Typed control protocol failure.
    #[error(transparent)]
    Protocol(#[from] ControlProtocolError),
    /// External action execution failure.
    #[error("master action {action:?} failed: {source}")]
    Action {
        action: ActionKind,
        #[source]
        source: ActionError,
    },
    /// Request preparation failed before dispatch.
    #[error("request preparation {step:?} failed: {source}")]
    Preparation {
        step: PreparationStep,
        #[source]
        source: PreparationError,
    },
    /// Process signaling or reaping failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Pure replacement invariant failure.
    #[error(transparent)]
    Replacement(#[from] ReplacementError),
}

impl MasterError {
    const fn is_issue_failure(&self) -> bool {
        matches!(self, Self::Action { .. } | Self::Preparation { .. })
    }
}
