#[allow(clippy::wildcard_imports)]
use super::*;

pub struct GenerationAdmission {
    pub(super) gate: AcceptGate,
    pub(super) _ownership: AcceptOwnership,
}

impl GenerationAdmission {
    #[must_use]
    pub fn accepting(&self) -> bool {
        self.gate.accepting()
    }
}

/// The closing of both admission gates of a generation, which a replacement completes or undoes as one.
pub struct AdmissionClose {
    pub(super) traffic: AcceptGateClose,
    pub(super) control: AcceptGateClose,
}

impl AdmissionClose {
    /// Whether every admission claimed before the close has ended within `timeout`.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        self.traffic.wait(timeout)
            && self
                .control
                .wait(deadline.saturating_duration_since(Instant::now()))
    }

    #[must_use]
    pub fn is_current(&self) -> bool {
        self.traffic.is_current() && self.control.is_current()
    }

    /// Opens both gates again; `false` when a later close or open superseded this one.
    #[must_use]
    pub fn reopen(self) -> bool {
        let traffic = self.traffic.reopen();
        let control = self.control.reopen();
        traffic && control
    }
}

pub struct GenerationReference {
    pub(super) generation: Arc<RuntimeGeneration>,
    pub(super) slot: usize,
}

pub struct GenerationMutation {
    pub(super) generation: Arc<RuntimeGeneration>,
}

impl GenerationMutation {
    #[must_use]
    pub fn generation(&self) -> &Arc<RuntimeGeneration> {
        &self.generation
    }
}

impl Drop for GenerationMutation {
    fn drop(&mut self) {
        self.generation.mutations.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub struct GenerationCandidate {
    pub(super) generation: Arc<RuntimeGeneration>,
    pub(super) id: u64,
}

impl GenerationCandidate {
    #[must_use]
    pub fn generation(&self) -> &Arc<RuntimeGeneration> {
        &self.generation
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        self.generation.revision()
    }
}

impl Drop for GenerationReference {
    fn drop(&mut self) {
        let before = self.generation.references[self.slot].fetch_sub(1, Ordering::AcqRel);
        if before == 1 && self.generation.retired() {
            Doorbell::process().ring();
        }
        self.generation.wake_drain();
    }
}

/// What a supervised worker knows of the worker it replaced.
pub(super) struct SupervisedLineage {
    pub(super) generation: u64,
    pub(super) replaced: Option<EffectiveRevision>,
}

#[derive(Default)]
pub(super) struct GenerationState {
    pub(super) active: Option<Arc<RuntimeGeneration>>,
    pub(super) candidate: Option<GenerationCandidate>,
    pub(super) disk_revision: Option<AuthoredRevision>,
    pub(super) last_failure: Option<&'static str>,
    pub(super) last_rejection: Option<ConfigRejection>,
    pub(super) previous: Option<Arc<RuntimeGeneration>>,
    pub(super) retired: RetiredGenerations,
    /// Retired workers of a supervising master, as it last reported them.
    pub(super) remote_retired: Vec<(EffectiveRevision, RemoteRetired, Instant)>,
    /// The revision a supervising master holds back while a retired worker drains.
    pub(super) remote_pending: Option<EffectiveRevision>,
    pub(super) restart_required: RestartRequired,
    pub(super) supervised_lineage: Option<SupervisedLineage>,
    pub(super) quarantined_revision: Option<EffectiveRevision>,
    pub(super) shutdown_generations: Vec<Arc<RuntimeGeneration>>,
    pub(super) shutting_down: bool,
    pub(super) starting_candidate: Option<CandidateStartReservation>,
}

impl GenerationState {
    pub(super) fn remote_pending_apply(&self) -> Option<PendingApply> {
        let revision = self.remote_pending.clone()?;
        let connections = self
            .remote_retired
            .iter()
            .map(|(_, report, _)| report.connections)
            .reduce(|total, connections| total + connections)?;
        Some(PendingApply {
            revision,
            reason: "retired_draining",
            connections: Some(connections),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateStartPhase {
    Starting,
    RuntimeOwned,
    Activating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CandidateStartReservation {
    pub(super) candidate_id: u64,
    pub(super) phase: CandidateStartPhase,
    pub(super) token: u64,
}

pub(super) struct ActivationReservation {
    pub(super) previous: Option<Arc<RuntimeGeneration>>,
    pub(super) previous_revision: Option<EffectiveRevision>,
    pub(super) runtime_owned: bool,
    pub(super) token: u64,
}

#[derive(Default)]
pub(super) struct GenerationCounters {
    pub(super) activations: AtomicU64,
    pub(super) config_rejections: AtomicU64,
    pub(super) failures: AtomicU64,
    pub(super) prepares: AtomicU64,
    pub(super) rollbacks: AtomicU64,
}

#[derive(Default)]
pub(super) struct OperationGate {
    available: Condvar,
    held: Mutex<bool>,
}

impl OperationGate {
    pub(super) fn acquire(self: &Arc<Self>) -> OperationAuthority {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *held {
            held = self
                .available
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *held = true;
        OperationAuthority {
            gate: Arc::clone(self),
        }
    }

    pub(super) fn acquire_until(self: &Arc<Self>, deadline: Instant) -> Option<OperationAuthority> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if !*held {
                *held = true;
                return Some(OperationAuthority {
                    gate: Arc::clone(self),
                });
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, _) = self
                .available
                .wait_timeout(held, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            held = next;
        }
    }
}

pub(super) struct OperationAuthority {
    gate: Arc<OperationGate>,
}

impl Drop for OperationAuthority {
    fn drop(&mut self) {
        let mut held = self
            .gate
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = false;
        drop(held);
        self.gate.available.notify_one();
    }
}
