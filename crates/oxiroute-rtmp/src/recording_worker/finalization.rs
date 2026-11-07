#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct PendingFinalization {
    pub(super) state: Arc<PendingFinalizationState>,
    pub(super) partial_exists: Arc<AtomicBool>,
}

pub(super) struct PendingFinalizationState {
    pub(super) result: Mutex<Option<Result<RecordingCommit, WorkerError>>>,
    pub(super) available: Condvar,
    pub(super) cancellation: RecordingCommitCancellation,
    pub(super) ticket: Mutex<Option<FinalizerTicket>>,
    pub(super) cancel_requested: AtomicBool,
    pub(super) claimed: AtomicBool,
    pub(super) finished: AtomicBool,
    pub(super) partial_relative_name: String,
}

impl Drop for PendingFinalization {
    fn drop(&mut self) {
        self.state.wait_until_finished();
    }
}

impl PendingFinalizationState {
    pub(super) fn cancel(&self) -> bool {
        self.cancel_requested.store(true, Ordering::Release);
        let commit_cancelled = self.cancellation.cancel();
        let queued_cancelled = self.cancel_queued();
        commit_cancelled || queued_cancelled
    }

    pub(super) fn set_ticket(&self, ticket: FinalizerTicket) {
        *self
            .ticket
            .lock()
            .expect("recorder finalization ticket mutex poisoned") = Some(ticket);
        if self.cancel_requested.load(Ordering::Acquire) {
            self.cancel_queued();
        }
    }

    fn cancel_queued(&self) -> bool {
        let removed = self
            .ticket
            .lock()
            .expect("recorder finalization ticket mutex poisoned")
            .as_ref()
            .is_some_and(FinalizerTicket::cancel_queued);
        if removed {
            assert!(
                self.try_claim(),
                "removed finalization job was already claimed"
            );
            self.complete(Err(WorkerError {
                kind: RecorderFailure::ShutdownTimedOut,
                recoverable_partial_name: Some(self.partial_relative_name.clone()),
                published_but_not_durable_relative_name: None,
            }));
        }
        removed
    }

    pub(super) fn try_claim(&self) -> bool {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn complete(&self, result: Result<RecordingCommit, WorkerError>) {
        *self
            .result
            .lock()
            .expect("recorder finalization result mutex poisoned") = Some(result);
        self.finished.store(true, Ordering::Release);
        self.available.notify_all();
    }

    pub(super) fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    fn wait_until_finished(&self) {
        let result = self
            .result
            .lock()
            .expect("recorder finalization result mutex poisoned");
        drop(
            self.available
                .wait_while(result, |_| !self.is_finished())
                .expect("recorder finalization result mutex poisoned while waiting"),
        );
    }

    pub(super) fn take_result(&self) -> Result<RecordingCommit, WorkerError> {
        let mut result = self
            .result
            .lock()
            .expect("recorder finalization result mutex poisoned");
        while !self.is_finished() {
            result = self
                .available
                .wait(result)
                .expect("recorder finalization result mutex poisoned while waiting");
        }
        result
            .take()
            .expect("finished recorder finalization retains its result")
    }
}
