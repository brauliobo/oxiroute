#[allow(clippy::wildcard_imports)]
use super::*;

/// `poll` acts on a passed deadline, but a shutdown deadline stays in force while the workers exit; waiting
/// on it again must not spin.
const DEADLINE_WAIT_FLOOR: Duration = Duration::from_millis(5);

impl<E: ActionExecutor> Master<E> {
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
        self.snapshot_history.worker_events(after, limit)
    }

    /// Builds a read-only snapshot from catalog state and identity-matched process observations.
    ///
    /// # Errors
    ///
    /// Returns an error when a current process occupies a role or identity not represented by the
    /// supplied catalog. Catalog launch documents and process observations remain separate in the
    /// returned projection.
    pub fn supervisor_snapshot<R: Clone, T>(
        &self,
        catalog: &SupervisedGenerationCatalog<R, T>,
    ) -> Result<SupervisorSnapshot<R>, SupervisorSnapshotError> {
        let sources = [
            (GenerationRole::Active, self.active.as_ref()),
            (GenerationRole::Candidate, self.candidate.as_ref()),
            (GenerationRole::Previous, self.retired.as_ref()),
        ]
        .into_iter()
        .filter_map(|(role, worker)| {
            worker
                .zip(worker.and_then(|worker| worker.status.as_ref()))
                .map(|(worker, status)| WorkerSnapshotSource {
                    role,
                    instance_id: &worker.instance_id,
                    generation_id: worker.generation,
                    status,
                })
        })
        .collect::<Vec<_>>();
        self.snapshot_history.snapshot(catalog, &sources)
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

    /// Blocks until a worker channel has a frame or closes, a worker exits, one of `extra` is readable,
    /// `timeout` elapses or the earliest state-machine deadline passes, whichever comes first.
    ///
    /// It consumes nothing: [`Self::poll`] follows. A signal that interrupts the wait ends it early.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout does not fit a poll timespec or polling fails.
    pub fn wait(&self, extra: &[BorrowedFd<'_>], timeout: Option<Duration>) -> io::Result<()> {
        // These stages have nothing to wait for: the caller acts on them.
        if matches!(
            self.stage,
            Stage::Failing { .. } | Stage::Failed | Stage::Stopped { .. }
        ) {
            return Ok(());
        }
        let mut descriptors: Vec<_> = extra
            .iter()
            .map(|descriptor| PollFd::from_borrowed_fd(*descriptor, PollFlags::IN))
            .collect();
        for worker in [&self.active, &self.candidate, &self.retired]
            .into_iter()
            .flatten()
        {
            let channel = worker
                .channel_open
                .then(|| worker.process.channel_descriptor())
                .flatten();
            descriptors.extend(
                channel
                    .into_iter()
                    .chain(worker.process.exit_descriptor())
                    .map(|descriptor| PollFd::from_borrowed_fd(descriptor, PollFlags::IN)),
            );
        }
        let now = Instant::now();
        let timeout = timeout
            .into_iter()
            .chain(self.next_deadline().map(|deadline| {
                deadline
                    .saturating_duration_since(now)
                    .max(DEADLINE_WAIT_FLOOR)
            }))
            .min()
            .map(Timespec::try_from)
            .transpose()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "wait timeout overflow"))?;
        match poll(&mut descriptors, timeout.as_ref()) {
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(error) => Err(error.into()),
        }
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
}

impl<E: ActionExecutor> Master<E> {
    pub(super) fn worker(&self, role: WorkerRole) -> Option<&ManagedWorker> {
        match role {
            WorkerRole::Active => self.active.as_ref(),
            WorkerRole::Candidate => self.candidate.as_ref(),
            WorkerRole::Retired => self.retired.as_ref(),
        }
    }

    pub(super) fn worker_mut(&mut self, role: WorkerRole) -> Option<&mut ManagedWorker> {
        match role {
            WorkerRole::Active => self.active.as_mut(),
            WorkerRole::Candidate => self.candidate.as_mut(),
            WorkerRole::Retired => self.retired.as_mut(),
        }
    }

    pub(super) fn take_worker(&mut self, role: WorkerRole) -> Option<ManagedWorker> {
        let worker = match role {
            WorkerRole::Active => self.active.take(),
            WorkerRole::Candidate => self.candidate.take(),
            WorkerRole::Retired => self.retired.take(),
        };
        self.snapshot_history
            .reap(worker.as_ref().and_then(|worker| worker.status.as_ref()));
        worker
    }

    pub(super) fn role_for(&self, instance_id: &InstanceId) -> Option<WorkerRole> {
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

    pub(super) fn worker_count(&self) -> usize {
        [
            self.active.as_ref(),
            self.candidate.as_ref(),
            self.retired.as_ref(),
        ]
        .into_iter()
        .flatten()
        .count()
    }

    pub(super) fn take_request_id(&mut self) -> Result<u64, PreparationError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .ok_or(PreparationError::RequestIdExhausted)?;
        Ok(request_id)
    }

    pub(super) fn supervisor_mut(&mut self) -> Result<&mut ReplacementSupervisor, MasterError> {
        self.supervisor
            .as_mut()
            .ok_or(MasterError::MissingSupervisor)
    }

    pub(super) fn active_id(&self) -> Result<&InstanceId, MasterError> {
        self.active_instance()
            .ok_or(MasterError::MissingWorker(WorkerRole::Active))
    }

    pub(super) fn candidate_id(&self) -> Result<&InstanceId, MasterError> {
        self.candidate
            .as_ref()
            .map(|worker| &worker.instance_id)
            .ok_or(MasterError::MissingWorker(WorkerRole::Candidate))
    }

    pub(super) fn retired_id(&self) -> Result<&InstanceId, MasterError> {
        self.retired
            .as_ref()
            .map(|worker| &worker.instance_id)
            .ok_or(MasterError::MissingWorker(WorkerRole::Retired))
    }

    pub(super) fn replacement_phase(&self) -> Option<ReplacementPhase> {
        self.supervisor.as_ref().map(ReplacementSupervisor::phase)
    }
}
