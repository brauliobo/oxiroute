#[allow(clippy::wildcard_imports)]
use super::*;

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
    pub(super) const fn is_issue_failure(&self) -> bool {
        matches!(self, Self::Action { .. } | Self::Preparation { .. })
    }
}
