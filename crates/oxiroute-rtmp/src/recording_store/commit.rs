#[allow(clippy::wildcard_imports)]
use super::*;

/// One durably published recording. The name is always relative and one component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingCommit {
    pub relative_name: String,
    pub bytes: u64,
}

pub(crate) struct RecordingResume {
    pub file: RecordingFile,
    pub flags: u8,
    pub last_timestamp_ms: u32,
}

pub(super) struct RecordingCommitState {
    pub(super) state: AtomicU8,
    #[cfg(test)]
    pub(super) gate: Mutex<Option<Arc<RecordingPublicationGate>>>,
    #[cfg(test)]
    pub(super) fail_partial_unlink: AtomicBool,
    #[cfg(test)]
    pub(super) fail_rollback_unlink: AtomicBool,
    #[cfg(test)]
    pub(super) fail_rollback_sync: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CommitPhase {
    Open,
    Cancelled,
    Publishing,
    Finished,
}

impl CommitPhase {
    pub(super) const fn encode(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Cancelled => 1,
            Self::Publishing => 2,
            Self::Finished => 3,
        }
    }

    fn decode(value: u8) -> Self {
        match value {
            0 => Self::Open,
            1 => Self::Cancelled,
            2 => Self::Publishing,
            3 => Self::Finished,
            _ => unreachable!("recording commit state is written only through CommitPhase"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RecordingCommitCancellation {
    pub(super) state: Arc<RecordingCommitState>,
}

#[cfg(test)]
pub(crate) struct RecordingPublicationGate {
    state: Mutex<RecordingPublicationGateState>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
struct RecordingPublicationGateState {
    progress: RecordingPublicationGateProgress,
    claim_allowed: bool,
    publication_allowed: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum RecordingPublicationGateProgress {
    Installed,
    BeforeClaim,
    AfterClaim,
}

impl RecordingCommitState {
    pub(super) fn cancelled(&self) -> bool {
        CommitPhase::decode(self.state.load(Ordering::Acquire)) == CommitPhase::Cancelled
    }

    pub(super) fn begin_publication(&self) -> bool {
        self.state
            .compare_exchange(
                CommitPhase::Open.encode(),
                CommitPhase::Publishing.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn finish(&self) {
        if self
            .state
            .compare_exchange(
                CommitPhase::Publishing.encode(),
                CommitPhase::Finished.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            let _ = self.state.compare_exchange(
                CommitPhase::Open.encode(),
                CommitPhase::Finished.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    #[cfg(test)]
    pub(super) fn wait_before_publication(&self) {
        let gate = self
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned")
            .clone();
        let Some(gate) = gate else {
            return;
        };
        let mut state = gate
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        state.progress = RecordingPublicationGateProgress::BeforeClaim;
        gate.changed.notify_all();
        while !state.claim_allowed && !self.cancelled() {
            state = gate
                .changed
                .wait(state)
                .expect("recording publication gate state mutex poisoned");
        }
    }

    #[cfg(test)]
    pub(super) fn wait_after_publication_claim(&self) {
        let gate = self
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned")
            .clone();
        let Some(gate) = gate else {
            return;
        };
        let mut state = gate
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        state.progress = RecordingPublicationGateProgress::AfterClaim;
        gate.changed.notify_all();
        while !state.publication_allowed {
            state = gate
                .changed
                .wait(state)
                .expect("recording publication gate state mutex poisoned");
        }
    }
}

#[cfg(test)]
pub(super) fn publication_partial_unlink_fault(commit: &RecordingCommitState) -> bool {
    commit.fail_partial_unlink.swap(false, Ordering::AcqRel)
}

#[cfg(not(test))]
pub(super) const fn publication_partial_unlink_fault(_: &RecordingCommitState) -> bool {
    false
}

#[cfg(test)]
pub(super) fn publication_rollback_unlink_fault(commit: &RecordingCommitState) -> bool {
    commit.fail_rollback_unlink.swap(false, Ordering::AcqRel)
}

#[cfg(not(test))]
pub(super) const fn publication_rollback_unlink_fault(_: &RecordingCommitState) -> bool {
    false
}

#[cfg(test)]
pub(super) fn publication_rollback_sync_fault(commit: &RecordingCommitState) -> bool {
    commit.fail_rollback_sync.swap(false, Ordering::AcqRel)
}

#[cfg(not(test))]
pub(super) const fn publication_rollback_sync_fault(_: &RecordingCommitState) -> bool {
    false
}

impl RecordingCommitCancellation {
    pub(crate) fn cancel(&self) -> bool {
        let cancelled = self
            .state
            .state
            .compare_exchange(
                CommitPhase::Open.encode(),
                CommitPhase::Cancelled.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        #[cfg(test)]
        if let Some(gate) = self
            .state
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned")
            .as_ref()
        {
            gate.changed.notify_all();
        }
        cancelled
    }

    pub(crate) fn finish(&self) {
        self.state.finish();
    }

    #[cfg(test)]
    pub(crate) fn install_publication_gate(&self) -> Arc<RecordingPublicationGate> {
        let gate = Arc::new(RecordingPublicationGate {
            state: Mutex::new(RecordingPublicationGateState {
                progress: RecordingPublicationGateProgress::Installed,
                claim_allowed: false,
                publication_allowed: false,
            }),
            changed: std::sync::Condvar::new(),
        });
        *self
            .state
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned") = Some(Arc::clone(&gate));
        gate
    }
}

#[cfg(test)]
impl RecordingPublicationGate {
    pub(crate) fn wait_before_claim(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        self.changed
            .wait_timeout_while(state, timeout, |state| {
                state.progress == RecordingPublicationGateProgress::Installed
            })
            .expect("recording publication gate state mutex poisoned")
            .0
            .progress
            != RecordingPublicationGateProgress::Installed
    }

    pub(crate) fn allow_claim(&self) {
        self.state
            .lock()
            .expect("recording publication gate state mutex poisoned")
            .claim_allowed = true;
        self.changed.notify_all();
    }

    pub(crate) fn wait_after_claim(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        self.changed
            .wait_timeout_while(state, timeout, |state| {
                state.progress != RecordingPublicationGateProgress::AfterClaim
            })
            .expect("recording publication gate state mutex poisoned")
            .0
            .progress
            == RecordingPublicationGateProgress::AfterClaim
    }

    pub(crate) fn allow_publication(&self) {
        self.state
            .lock()
            .expect("recording publication gate state mutex poisoned")
            .publication_allowed = true;
        self.changed.notify_all();
    }
}
