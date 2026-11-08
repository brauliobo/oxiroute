#[allow(clippy::wildcard_imports)]
use super::*;

/// One independent disk worker with a bounded try-enqueue media queue.
pub struct RecorderWorker {
    #[cfg(test)]
    default_arrival_origin: Instant,
    #[cfg(test)]
    default_arrival_unix_ms: u64,
    shared: Arc<WorkerShared>,
    thread: Option<JoinHandle<()>>,
}

#[must_use]
#[cfg(test)]
pub enum RecorderShutdown {
    Joined(RecorderWorkerStatus),
    TimedOut(RecorderWorkerSupervisor),
}

/// Retains ownership of a worker that exceeded its requested shutdown timeout.
pub struct RecorderWorkerSupervisor {
    shared: Arc<WorkerShared>,
    thread: Option<JoinHandle<()>>,
}

pub(super) struct WorkerShared {
    max_queue_messages: usize,
    max_queue_bytes: usize,
    record_mask: RecorderMediaMask,
    pub(super) max_size: Option<u64>,
    pub(super) max_frames: Option<u64>,
    pub(super) lock: bool,
    pub(super) append: bool,
    pub(super) notify: bool,
    pub(super) queue: Mutex<QueueState>,
    pub(super) available: Condvar,
    pub(super) status: Mutex<WorkerStatusState>,
    pub(super) bytes_written: Arc<AtomicU64>,
    pub(super) cancelled: AtomicBool,
    pub(super) discontinuity_requested: AtomicBool,
    pub(super) finished: Mutex<bool>,
    pub(super) finished_available: Condvar,
    shutdown_timeout: Duration,
    pub(super) commit_cancellation: Mutex<Option<RecordingCommitCancellation>>,
    pub(super) pending_finalizations: Mutex<Vec<Arc<PendingFinalizationState>>>,
    pub(super) current_partial_exists: Mutex<Option<Arc<AtomicBool>>>,
    pub(super) notification: Mutex<Option<RecorderNotification>>,
    #[cfg(test)]
    pub(super) panic_on_finish: AtomicBool,
    #[cfg(test)]
    pub(super) panic_after_finish: AtomicBool,
    #[cfg(test)]
    pub(super) fail_before_process: AtomicBool,
}

pub(super) struct QueueState {
    pub(super) events: VecDeque<QueuedMediaEvent>,
    pub(super) bytes: usize,
    pub(super) accepting: bool,
    pub(super) events_enqueued: u64,
    pub(super) events_dropped: u64,
    pub(super) discontinuities: u64,
    pub(super) discontinuity_pending: bool,
    pub(super) finalization_completed: bool,
}

pub(super) struct QueuedMediaEvent {
    pub(super) event: MediaEvent,
    pub(super) arrived_at: Instant,
    pub(super) arrived_at_unix_seconds: u64,
}

pub(super) struct WorkerStatusState {
    pub(super) phase: RecorderWorkerPhase,
    pub(super) current_relative_name: Option<String>,
    pub(super) last_completed_relative_name: Option<String>,
    pub(super) current_partial_name: Option<String>,
    pub(super) recoverable_partial_name: Option<String>,
    pub(super) published_but_not_durable_relative_name: Option<String>,
    pub(super) events_processed: u64,
    pub(super) segments_started: u64,
    pub(super) segments_completed: u64,
}

impl RecorderWorker {
    /// Starts a worker using a filename rendered by the existing recording path policy.
    ///
    /// The stream name is used only to render one bounded relative component. The worker receives
    /// no root path and performs no filesystem work on the enqueueing thread.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid queue/rotation limits, path rendering failure, or thread spawn
    /// failure.
    #[allow(clippy::too_many_lines)]
    pub fn start(
        store: RecordingStore,
        path_policy: &RecordingPathPolicy,
        stream_name: &[u8],
        opened_at_unix_seconds: u64,
        opened_at_utc: RecordingDateTime,
        config: RecorderWorkerConfig,
    ) -> Result<Self, RecorderWorkerStartError> {
        if config.max_queue_messages == 0 || config.max_queue_bytes == 0 {
            return Err(RecorderWorkerStartError::InvalidQueueLimits);
        }
        if config.shutdown_timeout.is_zero() {
            return Err(RecorderWorkerStartError::InvalidShutdownTimeout);
        }
        if !config.record_mask.audio && !config.record_mask.video {
            return Err(RecorderWorkerStartError::InvalidRecordMask);
        }
        if config.record_mask.keyframes && !config.record_mask.video {
            return Err(RecorderWorkerStartError::InvalidRecordMask);
        }
        if config.max_size.is_some_and(|maximum| maximum == 0)
            || config.max_frames.is_some_and(|maximum| maximum == 0)
        {
            return Err(RecorderWorkerStartError::InvalidRecordingLimit);
        }
        let rotation_interval_ms = config
            .rotation_interval
            .map(|interval| interval.as_millis())
            .map(|milliseconds| {
                if milliseconds == 0 || milliseconds > MAX_ROTATION_INTERVAL_MS {
                    Err(RecorderWorkerStartError::InvalidRotationInterval)
                } else {
                    u32::try_from(milliseconds)
                        .map_err(|_| RecorderWorkerStartError::InvalidRotationInterval)
                }
            })
            .transpose()?;
        path_policy.segment_filename(stream_name, opened_at_unix_seconds, opened_at_utc, 0)?;
        let lease = store
            .acquire_recorder()
            .map_err(RecorderWorkerStartError::Capacity)?;
        let path_policy = path_policy.clone();
        let stream_name = Arc::<[u8]>::from(stream_name);
        let setup = WorkerSetup {
            store,
            path_policy,
            stream_name,
            lease,
            rotation_interval: rotation_interval_ms
                .map(|value| Duration::from_millis(value.into())),
        };
        let shared = Arc::new(WorkerShared {
            max_queue_messages: config.max_queue_messages,
            max_queue_bytes: config.max_queue_bytes,
            record_mask: config.record_mask,
            max_size: config.max_size,
            max_frames: config.max_frames,
            lock: config.lock,
            append: config.append,
            notify: config.notify,
            queue: Mutex::new(QueueState {
                events: VecDeque::new(),
                bytes: 0,
                accepting: true,
                events_enqueued: 0,
                events_dropped: 0,
                discontinuities: 0,
                discontinuity_pending: false,
                finalization_completed: false,
            }),
            available: Condvar::new(),
            status: Mutex::new(WorkerStatusState {
                phase: RecorderWorkerPhase::Starting,
                current_relative_name: None,
                last_completed_relative_name: None,
                current_partial_name: None,
                recoverable_partial_name: None,
                published_but_not_durable_relative_name: None,
                events_processed: 0,
                segments_started: 0,
                segments_completed: 0,
            }),
            bytes_written: Arc::new(AtomicU64::new(0)),
            cancelled: AtomicBool::new(false),
            discontinuity_requested: AtomicBool::new(false),
            finished: Mutex::new(false),
            finished_available: Condvar::new(),
            shutdown_timeout: config.shutdown_timeout,
            commit_cancellation: Mutex::new(None),
            pending_finalizations: Mutex::new(Vec::new()),
            current_partial_exists: Mutex::new(None),
            notification: Mutex::new(None),
            #[cfg(test)]
            panic_on_finish: AtomicBool::new(false),
            #[cfg(test)]
            panic_after_finish: AtomicBool::new(false),
            #[cfg(test)]
            fail_before_process: AtomicBool::new(false),
        });
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("rtmp-recorder".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(&worker_shared, setup);
                }));
                if result.is_err() {
                    worker_shared.fail(WorkerError::new(RecorderFailure::WorkerPanicked));
                }
                worker_shared.mark_finished();
            })
            .map_err(RecorderWorkerStartError::ThreadSpawn)?;

        Ok(Self {
            #[cfg(test)]
            default_arrival_origin: Instant::now(),
            #[cfg(test)]
            default_arrival_unix_ms: opened_at_unix_seconds.saturating_mul(1_000),
            shared,
            thread: Some(thread),
        })
    }

    /// Attempts to enqueue one immutable media event without waiting for capacity or disk I/O.
    #[must_use]
    #[cfg(test)]
    pub fn try_enqueue(&self, event: MediaEvent) -> RecorderEnqueueResult {
        let elapsed = Duration::from_millis(u64::from(event.timestamp_ms()));
        self.try_enqueue_at(
            event,
            self.default_arrival_origin + elapsed,
            self.default_arrival_unix_ms
                .saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)),
        )
    }

    /// Enqueues media with independently supplied monotonic arrival and wall-clock observation.
    #[must_use]
    pub fn try_enqueue_at(
        &self,
        event: MediaEvent,
        arrived_at: Instant,
        at_unix_ms: u64,
    ) -> RecorderEnqueueResult {
        if !self.shared.record_mask.accepts(event.kind()) {
            return RecorderEnqueueResult::Filtered;
        }
        let mut queue = self.shared.lock_queue();
        if !queue.accepting {
            return RecorderEnqueueResult::Inactive;
        }
        let Some(queue_bytes) = queue.bytes.checked_add(event.payload_len()) else {
            self.shared.record_discontinuity(&mut queue);
            return RecorderEnqueueResult::DroppedDiscontinuity;
        };
        if queue.events.len() >= self.shared.max_queue_messages
            || queue_bytes > self.shared.max_queue_bytes
        {
            self.shared.record_discontinuity(&mut queue);
            return RecorderEnqueueResult::DroppedDiscontinuity;
        }

        queue.bytes = queue_bytes;
        queue.events.push_back(QueuedMediaEvent {
            event,
            arrived_at,
            arrived_at_unix_seconds: at_unix_ms / 1_000,
        });
        queue.events_enqueued = queue.events_enqueued.saturating_add(1);
        drop(queue);
        self.shared.available.notify_one();
        RecorderEnqueueResult::Queued
    }

    #[must_use]
    pub fn status(&self) -> RecorderWorkerStatus {
        self.shared.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn install_publication_gate(
        &self,
    ) -> Arc<crate::recording::recording_store::RecordingPublicationGate> {
        self.shared
            .commit_cancellation
            .lock()
            .expect("recorder commit cancellation mutex poisoned")
            .as_ref()
            .expect("active recorder segment has commit cancellation")
            .install_publication_gate()
    }

    #[cfg(test)]
    pub(crate) fn panic_on_finish(&self) {
        self.shared.panic_on_finish.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn panic_after_finish(&self) {
        self.shared
            .panic_after_finish
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn fail_before_process(&self) {
        self.shared
            .fail_before_process
            .store(true, Ordering::Release);
    }

    /// Stops admission and waits up to the configured timeout for the worker to join.
    ///
    /// A timeout returns a supervisor that still owns the active thread. Dropping the supervisor
    /// joins the thread rather than detaching it.
    #[cfg(test)]
    pub fn shutdown(mut self) -> RecorderShutdown {
        self.request_stop();
        if !self
            .shared
            .wait_until_finished(self.shared.shutdown_timeout)
        {
            self.request_cancel();
            return RecorderShutdown::TimedOut(RecorderWorkerSupervisor {
                shared: Arc::clone(&self.shared),
                thread: self.thread.take(),
            });
        }
        self.join_thread();
        RecorderShutdown::Joined(self.status())
    }

    pub(crate) fn into_supervisor(mut self) -> RecorderWorkerSupervisor {
        self.request_stop();
        RecorderWorkerSupervisor {
            shared: Arc::clone(&self.shared),
            thread: self.thread.take(),
        }
    }

    fn request_stop(&self) {
        let mut queue = self.shared.lock_queue();
        queue.accepting = false;
        drop(queue);
        self.shared.available.notify_all();
    }

    fn request_cancel(&self) {
        if self.shared.cancel() {
            self.shared
                .fail(WorkerError::new(RecorderFailure::ShutdownTimedOut));
        }
        self.shared.notify_cancelled();
    }

    fn join_thread(&mut self) {
        if self.thread.is_none() {
            return;
        }
        if self
            .thread
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            self.shared
                .fail(WorkerError::new(RecorderFailure::WorkerPanicked));
        }
    }
}

impl Drop for RecorderWorker {
    fn drop(&mut self) {
        if self.thread.is_none() {
            return;
        }
        self.request_stop();
        if !self
            .shared
            .wait_until_finished(self.shared.shutdown_timeout)
        {
            self.request_cancel();
        }
        self.join_thread();
    }
}

impl RecorderWorkerSupervisor {
    #[must_use]
    pub fn status(&self) -> RecorderWorkerStatus {
        self.shared.snapshot()
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.shared.is_finished()
    }

    #[must_use]
    pub fn join(mut self) -> RecorderWorkerStatus {
        self.join_thread();
        self.status()
    }

    pub(crate) fn shutdown_timeout(&self) -> Duration {
        self.shared.shutdown_timeout
    }

    pub(crate) fn cancel(&self) {
        if self.shared.cancel() {
            self.shared
                .fail(WorkerError::new(RecorderFailure::ShutdownTimedOut));
        }
        self.shared.notify_cancelled();
    }

    pub(crate) fn detach(mut self) -> RecorderWorkerStatus {
        drop(self.thread.take());
        self.status()
    }

    fn join_thread(&mut self) {
        if self
            .thread
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            self.shared
                .fail(WorkerError::new(RecorderFailure::WorkerPanicked));
        }
    }
}

impl Drop for RecorderWorkerSupervisor {
    fn drop(&mut self) {
        self.join_thread();
    }
}
