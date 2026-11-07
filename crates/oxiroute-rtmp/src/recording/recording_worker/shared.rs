#[allow(clippy::wildcard_imports)]
use super::*;

impl WorkerShared {
    pub(super) fn lock_queue(&self) -> MutexGuard<'_, QueueState> {
        self.queue.lock().expect("recorder queue mutex poisoned")
    }

    pub(super) fn lock_status(&self) -> MutexGuard<'_, WorkerStatusState> {
        self.status.lock().expect("recorder status mutex poisoned")
    }

    pub(super) fn notify_cancelled(&self) {
        self.available.notify_all();
    }

    pub(super) fn cancel(&self) -> bool {
        self.cancelled.store(true, Ordering::Release);
        let current = self
            .commit_cancellation
            .lock()
            .expect("recorder commit cancellation mutex poisoned")
            .clone();
        let pending = self
            .pending_finalizations
            .lock()
            .expect("recorder pending finalizations mutex poisoned")
            .clone();
        let has_commit = current.is_some() || !pending.is_empty();
        let mut commit_cancelled = current.is_some_and(|commit| commit.cancel());
        for pending in pending {
            commit_cancelled |= pending.cancel();
        }
        if has_commit {
            commit_cancelled
        } else {
            !matches!(
                self.lock_status().phase,
                RecorderWorkerPhase::Stopped | RecorderWorkerPhase::Failed(_)
            )
        }
    }

    pub(super) fn record_discontinuity(&self, queue: &mut QueueState) {
        let discarded = u64::try_from(queue.events.len()).unwrap_or(u64::MAX);
        queue.events_dropped = queue
            .events_dropped
            .saturating_add(discarded)
            .saturating_add(1);
        queue.discontinuities = queue.discontinuities.saturating_add(1);
        queue.events.clear();
        queue.bytes = 0;
        queue.accepting = false;
        queue.discontinuity_pending = true;
        self.discontinuity_requested.store(true, Ordering::Release);
        self.available.notify_all();
    }

    pub(super) fn record_failed_event(&self) {
        let mut queue = self.lock_queue();
        queue.events_dropped = queue.events_dropped.saturating_add(1);
    }

    pub(super) fn next_event(&self) -> NextEvent {
        let mut queue = self.lock_queue();
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return NextEvent::Finished;
            }
            if queue.discontinuity_pending {
                queue.discontinuity_pending = false;
                return NextEvent::Discontinuity;
            }
            if let Some(event) = queue.events.pop_front() {
                queue.bytes -= event.event.payload_len();
                return NextEvent::Event(event);
            }
            if !queue.accepting {
                return NextEvent::Finished;
            }
            queue = self
                .available
                .wait(queue)
                .expect("recorder queue mutex poisoned while waiting");
        }
    }

    pub(super) fn fail(&self, failure: WorkerError) {
        if !matches!(failure.kind, RecorderFailure::ShutdownTimedOut)
            && let Some(commit) = self
                .commit_cancellation
                .lock()
                .expect("recorder commit cancellation mutex poisoned")
                .as_ref()
        {
            commit.finish();
        }
        {
            let mut queue = self.lock_queue();
            queue.accepting = false;
            let discarded = u64::try_from(queue.events.len()).unwrap_or(u64::MAX);
            queue.events_dropped = queue.events_dropped.saturating_add(discarded);
            queue.events.clear();
            queue.bytes = 0;
        }
        let partial_exists = self
            .current_partial_exists
            .lock()
            .expect("recorder partial existence mutex poisoned")
            .as_ref()
            .is_some_and(|exists| exists.load(Ordering::Acquire));
        let mut status = self.lock_status();
        if !matches!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
        ) {
            status.phase = RecorderWorkerPhase::Failed(failure.kind);
        }
        if failure.recoverable_partial_name.is_some() && partial_exists {
            status.recoverable_partial_name = failure.recoverable_partial_name;
        } else if matches!(
            failure.kind,
            RecorderFailure::Write
                | RecorderFailure::Finalize
                | RecorderFailure::FileSync
                | RecorderFailure::Publish
                | RecorderFailure::Discontinuity
                | RecorderFailure::ShutdownTimedOut
                | RecorderFailure::WorkerPanicked
        ) {
            if partial_exists {
                status.recoverable_partial_name = status.current_partial_name.take();
            } else {
                status.current_partial_name = None;
            }
        }
        if failure.published_but_not_durable_relative_name.is_some() {
            status.published_but_not_durable_relative_name =
                failure.published_but_not_durable_relative_name;
        }
        status.current_relative_name = None;
        if self.notify {
            self.set_notification(RecorderNotification::Failed);
        }
        self.available.notify_all();
    }

    pub(super) fn snapshot(&self) -> RecorderWorkerStatus {
        let queue = self.lock_queue();
        let status = self.lock_status();
        RecorderWorkerStatus {
            phase: status.phase,
            current_relative_name: status.current_relative_name.clone(),
            last_completed_relative_name: status.last_completed_relative_name.clone(),
            recoverable_partial_name: status.recoverable_partial_name.clone(),
            published_but_not_durable_relative_name: status
                .published_but_not_durable_relative_name
                .clone(),
            queue_messages: queue.events.len(),
            queue_bytes: queue.bytes,
            events_enqueued: queue.events_enqueued,
            events_processed: status.events_processed,
            events_dropped: queue.events_dropped,
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            segments_started: status.segments_started,
            segments_completed: status.segments_completed,
            discontinuities: queue.discontinuities,
            last_notification: *self
                .notification
                .lock()
                .expect("recorder notification mutex poisoned"),
        }
    }

    pub(super) fn mark_finished(&self) {
        *self
            .finished
            .lock()
            .expect("recorder finished mutex poisoned") = true;
        self.finished_available.notify_all();
    }

    pub(super) fn wait_until_finished(&self, timeout: Duration) -> bool {
        let finished = self
            .finished
            .lock()
            .expect("recorder finished mutex poisoned");
        *self
            .finished_available
            .wait_timeout_while(finished, timeout, |finished| !*finished)
            .expect("recorder finished mutex poisoned while waiting")
            .0
    }

    pub(super) fn is_finished(&self) -> bool {
        *self
            .finished
            .lock()
            .expect("recorder finished mutex poisoned")
    }

    pub(super) fn stop_successfully(&self) {
        let mut status = self.lock_status();
        if !matches!(status.phase, RecorderWorkerPhase::Failed(_)) {
            status.phase = RecorderWorkerPhase::Stopped;
            status.current_relative_name = None;
            status.current_partial_name = None;
        }
        if self.notify {
            self.set_notification(RecorderNotification::Stopped);
        }
    }

    pub(super) fn set_notification(&self, notification: RecorderNotification) {
        *self
            .notification
            .lock()
            .expect("recorder notification mutex poisoned") = Some(notification);
    }
}

pub(super) enum NextEvent {
    Event(QueuedMediaEvent),
    Discontinuity,
    Finished,
}

pub(super) struct WorkerError {
    pub(super) kind: RecorderFailure,
    pub(super) recoverable_partial_name: Option<String>,
    pub(super) published_but_not_durable_relative_name: Option<String>,
}

impl WorkerError {
    pub(super) const fn new(kind: RecorderFailure) -> Self {
        Self {
            kind,
            recoverable_partial_name: None,
            published_but_not_durable_relative_name: None,
        }
    }

    pub(super) fn finalization(error: &RecordingStoreError) -> Self {
        let kind = match error {
            RecordingStoreError::FileSync { .. } => RecorderFailure::FileSync,
            RecordingStoreError::PartialOwnershipLost { .. }
            | RecordingStoreError::Publish { .. }
            | RecordingStoreError::PublishRollback { .. }
            | RecordingStoreError::PublishRollbackDirectorySync { .. }
            | RecordingStoreError::DescriptorPublicationUnsupported { .. }
            | RecordingStoreError::FinalNameCollisions { .. } => RecorderFailure::Publish,
            RecordingStoreError::PublishedDirectorySync { .. } => RecorderFailure::DirectorySync,
            RecordingStoreError::FinalizationCancelled { .. } => RecorderFailure::ShutdownTimedOut,
            _ => RecorderFailure::Finalize,
        };
        Self {
            kind,
            recoverable_partial_name: error.recoverable_partial_name().map(str::to_owned),
            published_but_not_durable_relative_name: error
                .published_recording()
                .map(|recording| recording.relative_name.clone()),
        }
    }
}
