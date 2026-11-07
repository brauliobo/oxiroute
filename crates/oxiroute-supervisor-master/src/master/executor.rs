#[allow(clippy::wildcard_imports)]
use super::*;

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
