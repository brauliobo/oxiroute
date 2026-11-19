#[allow(clippy::wildcard_imports)]
use super::*;

/// A master that hands its listeners to a successor keeps its worker only until that worker has
/// drained its connections.
impl<E: ActionExecutor> Master<E> {
    /// Duplicates the stable listener descriptors, in manifest order, for a successor master.
    ///
    /// # Errors
    ///
    /// Returns an error when a descriptor cannot be duplicated.
    pub fn export_listeners(&self) -> Result<Vec<OwnedFd>, ListenerOwnershipError> {
        self.listeners.duplicates()
    }

    /// Retires the active worker without a replacement: it stops accepting and keeps serving the
    /// connections it has until they end or `window` passes. The master then owns only draining
    /// workers and stops by itself once the last one is reaped.
    ///
    /// The successor master must already serve the listeners, or the listeners stop being served.
    ///
    /// # Errors
    ///
    /// Returns an error unless the master runs with no replacement in progress, or when the drain
    /// request cannot be sent.
    pub fn hand_over(&mut self, window: Duration, now: Instant) -> Result<(), MasterError> {
        if self.state() != MasterState::Running {
            return Err(MasterError::InvalidState(self.state()));
        }
        let instance_id = self.active_id()?.clone();
        let worker = self
            .active
            .take()
            .ok_or(MasterError::MissingWorker(WorkerRole::Active))?;
        self.retired = Some(worker);
        if let Some(worker) = self.retired.as_mut() {
            worker.retired = Some((now, window));
        }
        let issued = self.issue(WorkerRole::Retired, ControlPhase::Drain, now, window);
        let worker = self
            .retired
            .take()
            .ok_or(MasterError::MissingWorker(WorkerRole::Retired))?;
        self.detached.push(worker);
        self.supervisor = None;
        self.stage = Stage::HandedOver;
        self.events
            .push_back(MasterEvent::HandedOver { instance_id });
        if let Err(error) = issued {
            if error.is_issue_failure() {
                let index = self.detached.len() - 1;
                self.kill_detached(index, FailurePhase::Drain)?;
            } else {
                return Err(error);
            }
        }
        Ok(())
    }
}
