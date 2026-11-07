#[allow(clippy::wildcard_imports)]
use super::*;

pub struct GenerationAdmission {
    pub(super) generation: Arc<RuntimeGeneration>,
    pub(super) _ownership: AcceptOwnership,
}

impl GenerationAdmission {
    #[must_use]
    pub fn accepting(&self) -> bool {
        self.generation.accepting()
    }
}

pub struct GenerationReference {
    pub(super) generation: Arc<RuntimeGeneration>,
    pub(super) kind: RuntimeReferenceKind,
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
        self.generation.references[self.kind.index()].fetch_sub(1, Ordering::AcqRel);
        self.generation.drain.1.notify_all();
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
    pub(super) supervised_lineage: Option<SupervisedLineage>,
    pub(super) quarantined_revision: Option<EffectiveRevision>,
    pub(super) shutdown_generations: Vec<Arc<RuntimeGeneration>>,
    pub(super) shutting_down: bool,
    pub(super) starting_candidate: Option<CandidateStartReservation>,
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
