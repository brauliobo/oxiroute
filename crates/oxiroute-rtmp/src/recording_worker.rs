use std::{
    collections::VecDeque,
    io::{self, Seek, SeekFrom, Write},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    FlvMuxer, MediaEvent, MediaEventKind, RecorderLease, RecordingCommit, RecordingDateTime,
    RecordingFile, RecordingPathError, RecordingPathPolicy, RecordingStore, RecordingStoreError,
    RecordingTimeBasis,
    recording_store::{
        FinalizerTicket, MAX_PENDING_FINALIZATIONS_PER_RECORDER, RecordingCommitCancellation,
    },
};

const TIMESTAMP_HALF_RANGE: u32 = 1 << 31;
const MAX_ROTATION_INTERVAL_MS: u128 = (TIMESTAMP_HALF_RANGE - 1) as u128;

/// Queue and keyframe-aligned rotation bounds for one recorder worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecorderWorkerConfig {
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
    pub rotation_interval: Option<Duration>,
    pub shutdown_timeout: Duration,
    pub video_codec: Option<RecorderVideoCodec>,
    pub record_mask: RecorderMediaMask,
    pub append: bool,
    pub lock: bool,
    pub max_size: Option<u64>,
    pub max_frames: Option<u64>,
    pub notify: bool,
}

/// Exact track selection for one recorder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecorderMediaMask {
    pub audio: bool,
    pub video: bool,
    pub keyframes: bool,
}

impl Default for RecorderMediaMask {
    fn default() -> Self {
        Self {
            audio: true,
            video: true,
            keyframes: false,
        }
    }
}

impl RecorderMediaMask {
    #[must_use]
    pub const fn new(audio: bool, video: bool, keyframes: bool) -> Self {
        Self {
            audio,
            video,
            keyframes,
        }
    }

    #[must_use]
    pub const fn accepts(self, kind: MediaEventKind) -> bool {
        match kind {
            MediaEventKind::Metadata => self.audio || self.video,
            MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => self.audio,
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => self.video,
            MediaEventKind::VideoKeyframe => self.video,
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {
                self.video && !self.keyframes
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderVideoCodec {
    LegacyAvc,
    EnhancedAvc,
    Hevc,
    Av1,
}

impl Default for RecorderWorkerConfig {
    fn default() -> Self {
        Self {
            max_queue_messages: 256,
            max_queue_bytes: 8 * 1024 * 1024,
            rotation_interval: None,
            shutdown_timeout: Duration::from_secs(5),
            video_codec: None,
            record_mask: RecorderMediaMask::default(),
            append: false,
            lock: false,
            max_size: None,
            max_frames: None,
            notify: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderFailure {
    Open,
    Write,
    Finalize,
    FileSync,
    Publish,
    DirectorySync,
    Discontinuity,
    UnsupportedCodec,
    ShutdownTimedOut,
    WorkerPanicked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderNotification {
    Started,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderWorkerPhase {
    Starting,
    Recording,
    Stopped,
    Failed(RecorderFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderEnqueueResult {
    Queued,
    Filtered,
    DroppedDiscontinuity,
    Inactive,
}

/// Redacted worker state. It contains relative final names and categorical failures only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderWorkerStatus {
    pub phase: RecorderWorkerPhase,
    pub current_relative_name: Option<String>,
    pub last_completed_relative_name: Option<String>,
    pub recoverable_partial_name: Option<String>,
    pub published_but_not_durable_relative_name: Option<String>,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    pub events_enqueued: u64,
    pub events_processed: u64,
    pub events_dropped: u64,
    pub bytes_written: u64,
    pub segments_started: u64,
    pub segments_completed: u64,
    pub discontinuities: u64,
    pub last_notification: Option<RecorderNotification>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecorderWorkerStartError {
    #[error("recording path is invalid: {0}")]
    Path(#[from] RecordingPathError),
    #[error("recorder queue limits must both be nonzero")]
    InvalidQueueLimits,
    #[error(
        "recorder rotation interval must be between 1 and {MAX_ROTATION_INTERVAL_MS} milliseconds"
    )]
    InvalidRotationInterval,
    #[error("recorder worker thread cannot be started")]
    ThreadSpawn(#[source] io::Error),
    #[error("recorder capacity cannot be acquired")]
    Capacity(#[source] RecordingStoreError),
    #[error("recorder shutdown timeout must be nonzero")]
    InvalidShutdownTimeout,
    #[error("recorder track mask must include audio or video and keyframes require video")]
    InvalidRecordMask,
    #[error("recorder per-segment limits must be nonzero when configured")]
    InvalidRecordingLimit,
    #[error("recording does not support declared video codec {0:?}")]
    UnsupportedVideoCodec(RecorderVideoCodec),
}

/// One independent disk worker with a bounded try-enqueue media queue.
pub struct RecorderWorker {
    default_arrival_origin: Instant,
    default_arrival_unix_ms: u64,
    shared: Arc<WorkerShared>,
    thread: Option<JoinHandle<()>>,
}

#[must_use]
pub enum RecorderShutdown {
    Joined(RecorderWorkerStatus),
    TimedOut(RecorderWorkerSupervisor),
}

/// Retains ownership of a worker that exceeded its requested shutdown timeout.
pub struct RecorderWorkerSupervisor {
    shared: Arc<WorkerShared>,
    thread: Option<JoinHandle<()>>,
}

struct WorkerShared {
    max_queue_messages: usize,
    max_queue_bytes: usize,
    record_mask: RecorderMediaMask,
    max_size: Option<u64>,
    max_frames: Option<u64>,
    lock: bool,
    append: bool,
    notify: bool,
    queue: Mutex<QueueState>,
    available: Condvar,
    status: Mutex<WorkerStatusState>,
    bytes_written: Arc<AtomicU64>,
    cancelled: AtomicBool,
    discontinuity_requested: AtomicBool,
    finished: Mutex<bool>,
    finished_available: Condvar,
    shutdown_timeout: Duration,
    commit_cancellation: Mutex<Option<RecordingCommitCancellation>>,
    pending_finalizations: Mutex<Vec<Arc<PendingFinalizationState>>>,
    current_partial_exists: Mutex<Option<Arc<AtomicBool>>>,
    notification: Mutex<Option<RecorderNotification>>,
    #[cfg(test)]
    panic_on_finish: AtomicBool,
    #[cfg(test)]
    panic_after_finish: AtomicBool,
    #[cfg(test)]
    fail_before_process: AtomicBool,
}

struct QueueState {
    events: VecDeque<QueuedMediaEvent>,
    bytes: usize,
    accepting: bool,
    events_enqueued: u64,
    events_dropped: u64,
    discontinuities: u64,
    discontinuity_pending: bool,
}

struct QueuedMediaEvent {
    event: MediaEvent,
    arrived_at: Instant,
    arrived_at_unix_seconds: u64,
}

struct WorkerStatusState {
    phase: RecorderWorkerPhase,
    current_relative_name: Option<String>,
    last_completed_relative_name: Option<String>,
    current_partial_name: Option<String>,
    recoverable_partial_name: Option<String>,
    published_but_not_durable_relative_name: Option<String>,
    events_processed: u64,
    segments_started: u64,
    segments_completed: u64,
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
        if let Some(
            codec @ (RecorderVideoCodec::EnhancedAvc
            | RecorderVideoCodec::Hevc
            | RecorderVideoCodec::Av1),
        ) = config.video_codec
        {
            return Err(RecorderWorkerStartError::UnsupportedVideoCodec(codec));
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
            default_arrival_origin: Instant::now(),
            default_arrival_unix_ms: opened_at_unix_seconds.saturating_mul(1_000),
            shared,
            thread: Some(thread),
        })
    }

    /// Attempts to enqueue one immutable media event without waiting for capacity or disk I/O.
    #[must_use]
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
    ) -> Arc<crate::recording_store::RecordingPublicationGate> {
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

impl WorkerShared {
    fn lock_queue(&self) -> MutexGuard<'_, QueueState> {
        self.queue.lock().expect("recorder queue mutex poisoned")
    }

    fn lock_status(&self) -> MutexGuard<'_, WorkerStatusState> {
        self.status.lock().expect("recorder status mutex poisoned")
    }

    fn notify_cancelled(&self) {
        self.available.notify_all();
    }

    fn cancel(&self) -> bool {
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

    fn record_discontinuity(&self, queue: &mut QueueState) {
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

    fn record_failed_event(&self) {
        let mut queue = self.lock_queue();
        queue.events_dropped = queue.events_dropped.saturating_add(1);
    }

    fn next_event(&self) -> NextEvent {
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

    fn fail(&self, failure: WorkerError) {
        if !matches!(failure.kind, RecorderFailure::ShutdownTimedOut) {
            if let Some(commit) = self
                .commit_cancellation
                .lock()
                .expect("recorder commit cancellation mutex poisoned")
                .as_ref()
            {
                commit.finish();
            }
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

    fn snapshot(&self) -> RecorderWorkerStatus {
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

    fn mark_finished(&self) {
        *self
            .finished
            .lock()
            .expect("recorder finished mutex poisoned") = true;
        self.finished_available.notify_all();
    }

    fn wait_until_finished(&self, timeout: Duration) -> bool {
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

    fn is_finished(&self) -> bool {
        *self
            .finished
            .lock()
            .expect("recorder finished mutex poisoned")
    }

    fn stop_successfully(&self) {
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

    fn set_notification(&self, notification: RecorderNotification) {
        *self
            .notification
            .lock()
            .expect("recorder notification mutex poisoned") = Some(notification);
    }
}

enum NextEvent {
    Event(QueuedMediaEvent),
    Discontinuity,
    Finished,
}

struct WorkerError {
    kind: RecorderFailure,
    recoverable_partial_name: Option<String>,
    published_but_not_durable_relative_name: Option<String>,
}

impl WorkerError {
    const fn new(kind: RecorderFailure) -> Self {
        Self {
            kind,
            recoverable_partial_name: None,
            published_but_not_durable_relative_name: None,
        }
    }

    fn finalization(error: &RecordingStoreError) -> Self {
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

struct WorkerContext {
    shared: Arc<WorkerShared>,
    store: RecordingStore,
    path_policy: RecordingPathPolicy,
    stream_name: Arc<[u8]>,
    next_segment_sequence: u64,
    rotation_interval: Option<Duration>,
    segment: Option<Segment>,
    segment_final_name: Option<String>,
    segment_started_at: Option<Instant>,
    segment_started_at_unix_seconds: Option<u64>,
    latest_event_at_unix_seconds: u64,
    last_written_at_unix_seconds: u64,
    video_seen: bool,
    segment_headers: SegmentHeaders,
    pending_finalizations: VecDeque<PendingFinalization>,
    _lease: RecorderLease,
}

struct PendingFinalization {
    state: Arc<PendingFinalizationState>,
    partial_exists: Arc<AtomicBool>,
}

struct PendingFinalizationState {
    result: Mutex<Option<Result<RecordingCommit, WorkerError>>>,
    available: Condvar,
    cancellation: RecordingCommitCancellation,
    ticket: Mutex<Option<FinalizerTicket>>,
    cancel_requested: AtomicBool,
    claimed: AtomicBool,
    finished: AtomicBool,
    partial_relative_name: String,
}

impl Drop for PendingFinalization {
    fn drop(&mut self) {
        self.state.wait_until_finished();
    }
}

impl PendingFinalizationState {
    fn cancel(&self) -> bool {
        self.cancel_requested.store(true, Ordering::Release);
        let commit_cancelled = self.cancellation.cancel();
        let queued_cancelled = self.cancel_queued();
        commit_cancelled || queued_cancelled
    }

    fn set_ticket(&self, ticket: FinalizerTicket) {
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

    fn try_claim(&self) -> bool {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn complete(&self, result: Result<RecordingCommit, WorkerError>) {
        *self
            .result
            .lock()
            .expect("recorder finalization result mutex poisoned") = Some(result);
        self.finished.store(true, Ordering::Release);
        self.available.notify_all();
    }

    fn is_finished(&self) -> bool {
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

    fn take_result(&self) -> Result<RecordingCommit, WorkerError> {
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

struct WorkerSetup {
    store: RecordingStore,
    path_policy: RecordingPathPolicy,
    stream_name: Arc<[u8]>,
    lease: RecorderLease,
    rotation_interval: Option<Duration>,
}

fn run_worker(shared: &Arc<WorkerShared>, setup: WorkerSetup) {
    {
        let mut status = shared.lock_status();
        if !matches!(status.phase, RecorderWorkerPhase::Failed(_)) {
            status.phase = RecorderWorkerPhase::Recording;
        }
    }
    if shared.notify {
        shared.set_notification(RecorderNotification::Started);
    }
    let WorkerSetup {
        store,
        path_policy,
        stream_name,
        lease,
        rotation_interval,
    } = setup;
    let mut context = WorkerContext {
        shared: Arc::clone(shared),
        store,
        path_policy,
        stream_name,
        next_segment_sequence: 0,
        rotation_interval,
        segment: None,
        segment_final_name: None,
        segment_started_at: None,
        segment_started_at_unix_seconds: None,
        latest_event_at_unix_seconds: 0,
        last_written_at_unix_seconds: 0,
        video_seen: false,
        segment_headers: SegmentHeaders::default(),
        pending_finalizations: VecDeque::new(),
        _lease: lease,
    };

    loop {
        match shared.next_event() {
            NextEvent::Event(event) => {
                #[cfg(test)]
                if shared.fail_before_process.swap(false, Ordering::AcqRel) {
                    shared.record_failed_event();
                    shared.fail(WorkerError::new(RecorderFailure::Open));
                    return;
                }
                if let Err(failure) = context.process(&event) {
                    context.preserve_segment();
                    shared.record_failed_event();
                    shared.fail(failure);
                    return;
                }
                let mut status = shared.lock_status();
                status.events_processed = status.events_processed.saturating_add(1);
            }
            NextEvent::Discontinuity => {
                context.preserve_segment();
                shared.fail(WorkerError::new(RecorderFailure::Discontinuity));
                return;
            }
            NextEvent::Finished => break,
        }
    }
    if let Err(failure) = context.finish_segment() {
        context.preserve_segment();
        shared.fail(failure);
        return;
    }
    shared.stop_successfully();
}

impl WorkerContext {
    fn process(&mut self, queued: &QueuedMediaEvent) -> Result<(), WorkerError> {
        self.poll_finalization()?;
        let event = &queued.event;
        self.latest_event_at_unix_seconds = queued.arrived_at_unix_seconds;
        if is_unsupported_video_event(event) {
            return Err(WorkerError::new(RecorderFailure::UnsupportedCodec));
        }
        if matches!(
            event.kind(),
            MediaEventKind::AvcSequenceHeader
                | MediaEventKind::HevcSequenceHeader
                | MediaEventKind::Av1SequenceHeader
                | MediaEventKind::VideoKeyframe
                | MediaEventKind::VideoInterframe
                | MediaEventKind::VideoDisposable
        ) {
            self.video_seen = true;
        }

        if self.segment.is_some() {
            if self.limit_requires_rotation(event)? {
                self.start_segment_finalization()?;
                self.segment_started_at = None;
                self.segment_started_at_unix_seconds = None;
                self.open_segment(queued.arrived_at, queued.arrived_at_unix_seconds)?;
            } else if self.limit_requires_drop(event)? {
                return Ok(());
            }
        }

        if matches!(
            event.kind(),
            MediaEventKind::Metadata
                | MediaEventKind::AacSequenceHeader
                | MediaEventKind::AvcSequenceHeader
        ) {
            self.segment_headers.update(event.clone());
            if self.segment.is_none() {
                self.open_segment(queued.arrived_at, queued.arrived_at_unix_seconds)?;
            } else {
                self.segment
                    .as_mut()
                    .expect("segment was checked above")
                    .write(event)?;
            }
            self.last_written_at_unix_seconds = self.latest_event_at_unix_seconds;
            return Ok(());
        }

        if self.segment.is_none() {
            self.open_segment(queued.arrived_at, queued.arrived_at_unix_seconds)?;
        } else if self.should_rotate(event, queued.arrived_at) {
            self.start_segment_finalization()?;
            self.segment_started_at = None;
            self.segment_started_at_unix_seconds = None;
            self.open_segment(queued.arrived_at, queued.arrived_at_unix_seconds)?;
        }
        self.segment
            .as_mut()
            .expect("media processing opens a segment")
            .write(event)?;
        self.last_written_at_unix_seconds = self.latest_event_at_unix_seconds;
        Ok(())
    }

    fn limit_requires_rotation(&mut self, event: &MediaEvent) -> Result<bool, WorkerError> {
        let segment = self
            .segment
            .as_mut()
            .expect("limit checks require an open segment");
        let frame_limit_reached = self
            .shared
            .max_frames
            .is_some_and(|maximum| segment.frame_count() >= maximum && is_frame_event(event));
        let size_limit_reached = if let Some(maximum) = self.shared.max_size {
            segment
                .projected_end(event)?
                .is_some_and(|end| end > maximum)
        } else {
            false
        };
        let limit_reached = frame_limit_reached || size_limit_reached;
        Ok(limit_reached
            && segment.has_media()
            && (!self.video_seen || event.kind() == MediaEventKind::VideoKeyframe))
    }

    fn limit_requires_drop(&mut self, event: &MediaEvent) -> Result<bool, WorkerError> {
        let segment = self
            .segment
            .as_mut()
            .expect("limit checks require an open segment");
        let frame_limit_reached = self
            .shared
            .max_frames
            .is_some_and(|maximum| segment.frame_count() >= maximum && is_frame_event(event));
        let size_limit_reached = if let Some(maximum) = self.shared.max_size {
            segment
                .projected_end(event)?
                .is_some_and(|end| end > maximum)
        } else {
            false
        };
        Ok((frame_limit_reached || size_limit_reached)
            && segment.has_media()
            && self.video_seen
            && event.kind() != MediaEventKind::VideoKeyframe)
    }

    fn open_segment(
        &mut self,
        arrived_at: Instant,
        arrived_at_unix_seconds: u64,
    ) -> Result<(), WorkerError> {
        let headers = self.segment_headers.ordered();
        let resume = if self.next_segment_sequence == 0 {
            self.resumable_segment(arrived_at_unix_seconds)?
        } else {
            None
        };
        let (
            segment_name,
            segment,
            segment_started_at,
            segment_started_at_unix_seconds,
            segment_sequence,
        ) = if let Some((segment_name, started_at_unix_seconds, sequence)) = resume {
            let segment = Segment::resume(
                &self.store,
                &segment_name,
                &headers,
                Arc::clone(&self.shared.bytes_written),
                &self.shared,
            )?;
            let elapsed = Duration::from_secs(
                arrived_at_unix_seconds.saturating_sub(started_at_unix_seconds),
            );
            (
                segment_name,
                segment,
                arrived_at.checked_sub(elapsed).unwrap_or(arrived_at),
                started_at_unix_seconds,
                sequence,
            )
        } else {
            let requested_name = self
                .path_policy
                .segment_filename(
                    &self.stream_name,
                    arrived_at_unix_seconds,
                    RecordingDateTime::from_unix_seconds(arrived_at_unix_seconds)
                        .map_err(|_| WorkerError::new(RecorderFailure::Open))?,
                    self.next_segment_sequence,
                )
                .map_err(|_| WorkerError::new(RecorderFailure::Open))?;
            let segment = Segment::open(
                &self.store,
                &requested_name,
                &headers,
                Arc::clone(&self.shared.bytes_written),
                &self.shared,
            )?;
            let segment_name = segment.partial_relative_name.clone();
            (
                segment_name,
                segment,
                arrived_at,
                arrived_at_unix_seconds,
                self.next_segment_sequence,
            )
        };
        let next_segment_sequence = segment_sequence
            .checked_add(1)
            .ok_or_else(|| WorkerError::new(RecorderFailure::Open))?;
        if self.shared.discontinuity_requested.load(Ordering::Acquire) {
            return Err(WorkerError::new(RecorderFailure::Discontinuity));
        }
        if self.shared.cancelled.load(Ordering::Acquire) {
            return Err(WorkerError::new(RecorderFailure::ShutdownTimedOut));
        }
        self.segment = Some(segment);
        self.segment_final_name = Some(segment_name.clone());
        *self
            .shared
            .current_partial_exists
            .lock()
            .expect("recorder partial existence mutex poisoned") = self
            .segment
            .as_ref()
            .map(|segment| Arc::clone(&segment.partial_exists));
        self.segment_started_at = Some(segment_started_at);
        self.segment_started_at_unix_seconds = Some(segment_started_at_unix_seconds);
        self.next_segment_sequence = next_segment_sequence;
        let mut status = self.shared.lock_status();
        if !matches!(status.phase, RecorderWorkerPhase::Failed(_)) {
            status.phase = RecorderWorkerPhase::Recording;
            status.current_relative_name = Some(segment_name);
            status.current_partial_name = self
                .segment
                .as_ref()
                .map(|segment| segment.partial_relative_name.clone());
            status.segments_started = status.segments_started.saturating_add(1);
        }
        Ok(())
    }

    fn resumable_segment(
        &self,
        arrived_at_unix_seconds: u64,
    ) -> Result<Option<(String, u64, u64)>, WorkerError> {
        let names = self
            .store
            .recording_names()
            .map_err(|_| WorkerError::new(RecorderFailure::Open))?;
        if self.shared.append {
            let requested = self
                .path_policy
                .segment_filename(
                    &self.stream_name,
                    arrived_at_unix_seconds,
                    RecordingDateTime::from_unix_seconds(arrived_at_unix_seconds)
                        .map_err(|_| WorkerError::new(RecorderFailure::Open))?,
                    0,
                )
                .map_err(|_| WorkerError::new(RecorderFailure::Open))?;
            if names.iter().any(|name| name == &requested) {
                return Ok(Some((requested, arrived_at_unix_seconds, 0)));
            }
        }
        let Some(interval) = self.rotation_interval else {
            return Ok(None);
        };
        Ok(names
            .into_iter()
            .filter_map(|name| {
                let (started_at, sequence, collision) = self
                    .path_policy
                    .segment_identity_from_filename(&self.stream_name, &name)?;
                let age = arrived_at_unix_seconds.checked_sub(started_at)?;
                (Duration::from_secs(age) < interval)
                    .then_some((name, started_at, sequence, collision))
            })
            .max_by_key(|(_, started_at, sequence, collision)| (*started_at, *sequence, *collision))
            .map(|(name, started_at, sequence, _)| (name, started_at, sequence)))
    }

    fn prepare_segment_finish(&mut self) -> Result<Option<PreparedSegment>, WorkerError> {
        let Some(segment) = self.segment.as_ref() else {
            return Ok(None);
        };
        segment.preserve();
        #[cfg(test)]
        assert!(
            !self.shared.panic_on_finish.swap(false, Ordering::AcqRel),
            "injected recorder segment finish panic"
        );
        let sequence = self.next_segment_sequence.saturating_sub(1);
        let naming_time = match self.path_policy.time_basis() {
            RecordingTimeBasis::SegmentStart => self
                .segment_started_at_unix_seconds
                .expect("an open segment retains its wall-clock start"),
            RecordingTimeBasis::SegmentEnd => self.last_written_at_unix_seconds,
        };
        let opened_final_name = self
            .segment_final_name
            .take()
            .expect("an open segment retains its exact final name");
        let final_name = if self.path_policy.time_basis() == RecordingTimeBasis::SegmentStart {
            opened_final_name
        } else {
            self.path_policy
                .segment_filename(
                    &self.stream_name,
                    naming_time,
                    RecordingDateTime::from_unix_seconds(naming_time)
                        .map_err(|_| WorkerError::new(RecorderFailure::Finalize))?,
                    sequence,
                )
                .map_err(|_| WorkerError::new(RecorderFailure::Finalize))?
        };
        let segment = self
            .segment
            .take()
            .expect("segment remains owned through final-name rendering");
        segment.prepare_finish(final_name).map(Some)
    }

    fn start_segment_finalization(&mut self) -> Result<(), WorkerError> {
        self.poll_finalization()?;
        if self.pending_finalizations.len() >= MAX_PENDING_FINALIZATIONS_PER_RECORDER {
            return Err(WorkerError::new(RecorderFailure::Finalize));
        }
        let Some(segment) = self.prepare_segment_finish()? else {
            return Ok(());
        };
        let cancellation = self
            .shared
            .commit_cancellation
            .lock()
            .expect("recorder commit cancellation mutex poisoned")
            .take()
            .expect("prepared segment retains commit cancellation");
        let partial_relative_name = segment.partial_relative_name.clone();
        let partial_exists = Arc::clone(&segment.partial_exists);
        let state = Arc::new(PendingFinalizationState {
            result: Mutex::new(None),
            available: Condvar::new(),
            cancellation,
            ticket: Mutex::new(None),
            cancel_requested: AtomicBool::new(false),
            claimed: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            partial_relative_name: partial_relative_name.clone(),
        });
        self.shared
            .pending_finalizations
            .lock()
            .expect("recorder pending finalizations mutex poisoned")
            .push(Arc::clone(&state));
        if self.shared.cancelled.load(Ordering::Acquire) {
            let _ = state.cancel();
        }
        let completion = Arc::clone(&state);
        let panic_partial_relative_name = partial_relative_name.clone();
        let ticket = self.store.submit_finalization(Box::new(move || {
            if !completion.try_claim() {
                return;
            }
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| segment.finish()))
                    .unwrap_or(Err(WorkerError {
                        kind: RecorderFailure::WorkerPanicked,
                        recoverable_partial_name: Some(panic_partial_relative_name),
                        published_but_not_durable_relative_name: None,
                    }));
            completion.complete(result);
        }));
        state.set_ticket(ticket);
        self.pending_finalizations.push_back(PendingFinalization {
            state,
            partial_exists,
        });
        self.clear_current_segment_status();
        Ok(())
    }

    fn poll_finalization(&mut self) -> Result<(), WorkerError> {
        while self
            .pending_finalizations
            .front()
            .is_some_and(|pending| pending.state.is_finished())
        {
            self.complete_pending_finalization(false)?;
        }
        Ok(())
    }

    fn complete_pending_finalization(&mut self, wait: bool) -> Result<(), WorkerError> {
        let Some(pending) = self.pending_finalizations.front() else {
            return Ok(());
        };
        if !wait && !pending.state.is_finished() {
            return Ok(());
        }
        let pending = self
            .pending_finalizations
            .pop_front()
            .expect("pending finalization was checked above");
        let result = pending.state.take_result();
        self.shared
            .pending_finalizations
            .lock()
            .expect("recorder pending finalizations mutex poisoned")
            .retain(|state| !Arc::ptr_eq(state, &pending.state));
        let commit = match result {
            Ok(commit) => commit,
            Err(error) => {
                *self
                    .shared
                    .current_partial_exists
                    .lock()
                    .expect("recorder partial existence mutex poisoned") =
                    Some(Arc::clone(&pending.partial_exists));
                return Err(error);
            }
        };
        self.record_completed_segment(commit);
        Ok(())
    }

    fn finish_segment(&mut self) -> Result<(), WorkerError> {
        while !self.pending_finalizations.is_empty() {
            self.complete_pending_finalization(true)?;
        }
        self.start_segment_finalization()?;
        while !self.pending_finalizations.is_empty() {
            self.complete_pending_finalization(true)?;
        }
        Ok(())
    }

    fn clear_current_segment_status(&self) {
        *self
            .shared
            .current_partial_exists
            .lock()
            .expect("recorder partial existence mutex poisoned") = None;
        let mut status = self.shared.lock_status();
        status.current_relative_name = None;
        status.current_partial_name = None;
    }

    fn record_completed_segment(&self, commit: RecordingCommit) {
        #[cfg(test)]
        assert!(
            !self.shared.panic_after_finish.swap(false, Ordering::AcqRel),
            "injected recorder post-finish panic"
        );
        let mut status = self.shared.lock_status();
        status.last_completed_relative_name = Some(commit.relative_name);
        status.segments_completed = status.segments_completed.saturating_add(1);
    }

    fn preserve_segment(&self) {
        if let Some(segment) = &self.segment {
            segment.preserve();
        }
    }

    fn should_rotate(&self, event: &MediaEvent, arrived_at: Instant) -> bool {
        let (Some(interval), Some(started_at)) = (self.rotation_interval, self.segment_started_at)
        else {
            return false;
        };
        if arrived_at.saturating_duration_since(started_at) < interval {
            return false;
        }
        if self.video_seen {
            event.kind() == MediaEventKind::VideoKeyframe
        } else {
            event.kind() == MediaEventKind::Audio
        }
    }
}

fn is_unsupported_video_event(event: &MediaEvent) -> bool {
    matches!(
        event.kind(),
        MediaEventKind::HevcSequenceHeader | MediaEventKind::Av1SequenceHeader
    ) || matches!(
        event.kind(),
        MediaEventKind::AvcSequenceHeader
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable
    ) && event
        .payload()
        .first()
        .is_some_and(|header| header & 0x80 != 0)
}

fn is_frame_event(event: &MediaEvent) -> bool {
    matches!(
        event.kind(),
        MediaEventKind::Audio
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable
    )
}

struct Segment {
    muxer: FlvMuxer<CountedRecordingFile>,
    partial_relative_name: String,
    preserve_partial: Arc<AtomicBool>,
    partial_exists: Arc<AtomicBool>,
    frame_count: u64,
}

struct PreparedSegment {
    file: CountedRecordingFile,
    partial_relative_name: String,
    partial_exists: Arc<AtomicBool>,
}

impl Segment {
    fn open(
        store: &RecordingStore,
        relative_name: &str,
        headers: &[MediaEvent],
        bytes_written: Arc<AtomicU64>,
        shared: &WorkerShared,
    ) -> Result<Self, WorkerError> {
        let file = store
            .create_unless_with_options(
                relative_name,
                || {
                    shared.cancelled.load(Ordering::Acquire)
                        || shared.discontinuity_requested.load(Ordering::Acquire)
                },
                shared.lock,
                shared.max_size,
            )
            .map_err(|error| {
                if matches!(error, RecordingStoreError::CreationCancelled) {
                    if shared.discontinuity_requested.load(Ordering::Acquire) {
                        WorkerError::new(RecorderFailure::Discontinuity)
                    } else {
                        WorkerError::new(RecorderFailure::ShutdownTimedOut)
                    }
                } else if matches!(
                    error,
                    RecordingStoreError::FinalNameCollisions { .. }
                        | RecordingStoreError::PartialNameCollisions
                ) {
                    WorkerError::new(RecorderFailure::Publish)
                } else {
                    WorkerError::new(RecorderFailure::Open)
                }
            })?;
        if shared.discontinuity_requested.load(Ordering::Acquire) {
            return Err(WorkerError::new(RecorderFailure::Discontinuity));
        }
        if shared.cancelled.load(Ordering::Acquire) {
            return Err(WorkerError::new(RecorderFailure::ShutdownTimedOut));
        }
        let partial_relative_name = file.partial_relative_name().to_owned();
        let preserve_partial = file.preservation_handle();
        let partial_exists = file.partial_existence_handle();
        *shared
            .commit_cancellation
            .lock()
            .expect("recorder commit cancellation mutex poisoned") =
            Some(file.commit_cancellation());
        let counted = CountedRecordingFile {
            inner: file,
            bytes_written,
        };
        let mut muxer =
            FlvMuxer::new(counted).map_err(|_| WorkerError::new(RecorderFailure::Open))?;
        for header in headers {
            write_to_muxer(&mut muxer, header)?;
        }
        Ok(Self {
            muxer,
            partial_relative_name,
            preserve_partial,
            partial_exists,
            frame_count: 0,
        })
    }

    fn resume(
        store: &RecordingStore,
        relative_name: &str,
        headers: &[MediaEvent],
        bytes_written: Arc<AtomicU64>,
        shared: &WorkerShared,
    ) -> Result<Self, WorkerError> {
        let resume = store
            .resume_with_options(relative_name, shared.lock, shared.max_size)
            .map_err(|_| WorkerError::new(RecorderFailure::Open))?;
        let partial_relative_name = resume.file.partial_relative_name().to_owned();
        let preserve_partial = resume.file.preservation_handle();
        let partial_exists = resume.file.partial_existence_handle();
        *shared
            .commit_cancellation
            .lock()
            .expect("recorder commit cancellation mutex poisoned") =
            Some(resume.file.commit_cancellation());
        let counted = CountedRecordingFile {
            inner: resume.file,
            bytes_written,
        };
        let mut muxer = FlvMuxer::resume(counted, resume.flags, resume.last_timestamp_ms);
        for header in headers {
            write_to_muxer(&mut muxer, header)?;
        }
        Ok(Self {
            muxer,
            partial_relative_name,
            preserve_partial,
            partial_exists,
            frame_count: 0,
        })
    }

    fn write(&mut self, event: &MediaEvent) -> Result<(), WorkerError> {
        let projected = self.projected_bytes(event)?;
        write_to_muxer(&mut self.muxer, event).map(|()| {
            if projected > 0 && is_frame_event(event) {
                self.frame_count = self.frame_count.saturating_add(1);
            }
        })
    }

    fn projected_end(&mut self, event: &MediaEvent) -> Result<Option<u64>, WorkerError> {
        let current = self
            .muxer
            .output_position()
            .map_err(|_| WorkerError::new(RecorderFailure::Write))?;
        let projected = self.projected_bytes(event)?;
        Ok(current.checked_add(projected))
    }

    fn projected_bytes(&self, event: &MediaEvent) -> Result<u64, WorkerError> {
        let projected = match event.kind() {
            MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => {
                self.muxer.projected_audio_size(event.payload())
            }
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => self.muxer.projected_video_size(event.payload()),
            MediaEventKind::Metadata => self.muxer.projected_metadata_size(event.payload()),
        };
        projected.map_err(|_| WorkerError::new(RecorderFailure::Write))
    }

    fn has_media(&self) -> bool {
        self.muxer.has_media()
    }

    const fn frame_count(&self) -> u64 {
        self.frame_count
    }

    fn preserve(&self) {
        self.preserve_partial.store(true, Ordering::Release);
    }

    fn prepare_finish(self, final_name: String) -> Result<PreparedSegment, WorkerError> {
        self.preserve_partial.store(true, Ordering::Release);
        let mut file = self.muxer.close().map_err(|_| WorkerError {
            kind: RecorderFailure::Finalize,
            recoverable_partial_name: Some(self.partial_relative_name.clone()),
            published_but_not_durable_relative_name: None,
        })?;
        file.inner
            .set_final_relative_name(final_name)
            .map_err(|error| WorkerError::finalization(&error))?;
        Ok(PreparedSegment {
            file,
            partial_relative_name: self.partial_relative_name,
            partial_exists: self.partial_exists,
        })
    }
}

impl PreparedSegment {
    fn finish(self) -> Result<RecordingCommit, WorkerError> {
        self.file
            .commit()
            .map_err(|error| WorkerError::finalization(&error))
    }
}

fn write_to_muxer(
    muxer: &mut FlvMuxer<CountedRecordingFile>,
    event: &MediaEvent,
) -> Result<(), WorkerError> {
    match event.kind() {
        MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => muxer
            .write_audio(event.timestamp_ms(), event.payload())
            .map_err(|_| WorkerError::new(RecorderFailure::Write)),
        MediaEventKind::AvcSequenceHeader
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => muxer
            .write_video(event.timestamp_ms(), event.payload())
            .map_err(|_| WorkerError::new(RecorderFailure::Write)),
        MediaEventKind::HevcSequenceHeader | MediaEventKind::Av1SequenceHeader => {
            Err(WorkerError::new(RecorderFailure::Write))
        }
        MediaEventKind::Metadata => muxer
            .write_metadata(event.payload())
            .map_err(|_| WorkerError::new(RecorderFailure::Write)),
    }
}

struct CountedRecordingFile {
    inner: RecordingFile,
    bytes_written: Arc<AtomicU64>,
}

impl CountedRecordingFile {
    fn commit(self) -> Result<RecordingCommit, RecordingStoreError> {
        self.inner.commit()
    }
}

impl Write for CountedRecordingFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let before = self.inner.bytes_written();
        let written = self.inner.write(buffer)?;
        let growth = self.inner.bytes_written() - before;
        self.bytes_written.fetch_add(growth, Ordering::Relaxed);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for CountedRecordingFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[derive(Default)]
struct SegmentHeaders {
    sequence: u64,
    metadata: Option<CachedHeader>,
    aac: Option<CachedHeader>,
    avc: Option<CachedHeader>,
}

struct CachedHeader {
    sequence: u64,
    event: MediaEvent,
}

impl SegmentHeaders {
    fn update(&mut self, event: MediaEvent) {
        self.sequence = self.sequence.saturating_add(1);
        let header = CachedHeader {
            sequence: self.sequence,
            event,
        };
        match header.event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(header),
            MediaEventKind::AacSequenceHeader => self.aac = Some(header),
            MediaEventKind::AvcSequenceHeader => self.avc = Some(header),
            _ => unreachable!("only replayable segment headers are cached"),
        }
    }

    fn ordered(&self) -> Vec<MediaEvent> {
        let mut headers: Vec<_> = self
            .metadata
            .iter()
            .chain(self.aac.iter())
            .chain(self.avc.iter())
            .collect();
        headers.sort_by_key(|header| header.sequence);
        headers
            .into_iter()
            .map(|header| header.event.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread, time::Duration};

    use tempfile::tempdir;

    use super::*;
    use crate::RecordingStoreLimits;

    #[test]
    fn rotation_drains_media_while_the_previous_segment_is_publishing() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let worker = RecorderWorker::start(
            store.clone(),
            &RecordingPathPolicy::new(".flv", false).expect("path policy"),
            b"camera",
            1_721_619_000,
            RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
            RecorderWorkerConfig {
                max_queue_messages: 8,
                max_queue_bytes: 1024,
                rotation_interval: Some(Duration::from_millis(1)),
                shutdown_timeout: Duration::from_secs(1),
                video_codec: None,
                ..RecorderWorkerConfig::default()
            },
        )
        .expect("recorder worker");
        enqueue(
            &worker,
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\0\x12"[..])).unwrap(),
        );
        enqueue(
            &worker,
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
        );
        wait_until(|| worker.status().events_processed == 2);
        let gate = worker.install_publication_gate();

        enqueue(
            &worker,
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
        );
        assert!(gate.wait_before_claim(Duration::from_secs(1)));
        wait_until(|| worker.status().segments_started == 2);
        assert_eq!(store.stats().active_recorders, 1);
        enqueue(
            &worker,
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x33"[..])).unwrap(),
        );
        wait_until(|| worker.status().events_processed == 4);
        assert_eq!(worker.status().discontinuities, 0);

        gate.allow_claim();
        assert!(gate.wait_after_claim(Duration::from_secs(1)));
        gate.allow_publication();
        let status = match worker.shutdown() {
            RecorderShutdown::Joined(status) => status,
            RecorderShutdown::TimedOut(supervisor) => {
                panic!("recorder shutdown timed out: {:?}", supervisor.status())
            }
        };
        assert_eq!(status.segments_completed, 2);
        assert_eq!(status.discontinuities, 0);
        assert_eq!(store.stats().active_recorders, 0);
        assert_eq!(
            fs::read_dir(root.path())
                .expect("recording entries")
                .filter_map(Result::ok)
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .count(),
            2
        );
    }

    #[test]
    fn ten_aligned_recorders_drain_media_while_root_finalization_is_serialized() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(16 * 1024 * 1024),
                max_files: Some(40),
                max_active_recorders: 10,
            },
        )
        .expect("recording store");
        let path = RecordingPathPolicy::new(".flv", false).expect("path policy");
        let config = RecorderWorkerConfig {
            max_queue_messages: 64,
            max_queue_bytes: 1024 * 1024,
            rotation_interval: Some(Duration::from_millis(1)),
            shutdown_timeout: Duration::from_secs(2),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        };
        let workers: Vec<_> = (0..10)
            .map(|index| {
                RecorderWorker::start(
                    store.clone(),
                    &path,
                    format!("camera-{index}").as_bytes(),
                    1_721_619_000,
                    RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
                    config,
                )
                .expect("recorder worker")
            })
            .collect();
        for worker in &workers {
            enqueue(
                worker,
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
            );
            wait_until(|| worker.status().events_processed == 1);
        }
        let gate = workers[0].install_publication_gate();

        enqueue(
            &workers[0],
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
        );
        assert!(gate.wait_before_claim(Duration::from_secs(1)));
        for worker in &workers[1..] {
            enqueue(
                worker,
                MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
            );
        }
        for worker in &workers {
            wait_until(|| worker.status().segments_started == 2);
            enqueue_audio_burst(worker, 1, 16);
        }
        for worker in &workers {
            wait_until(|| worker.status().events_processed == 18);
            assert_eq!(worker.status().discontinuities, 0);
        }
        for worker in &workers {
            enqueue(
                worker,
                MediaEvent::audio(2, Arc::<[u8]>::from(&b"\xaf\x01\x44"[..])).unwrap(),
            );
            wait_until(|| worker.status().segments_started == 3);
            enqueue_audio_burst(worker, 2, 16);
        }
        for worker in &workers {
            wait_until(|| worker.status().events_processed == 35);
            assert_eq!(worker.status().discontinuities, 0);
        }

        gate.allow_claim();
        assert!(gate.wait_after_claim(Duration::from_secs(1)));
        gate.allow_publication();
        for worker in workers {
            let status = match worker.shutdown() {
                RecorderShutdown::Joined(status) => status,
                RecorderShutdown::TimedOut(supervisor) => {
                    panic!("aligned recorder timed out: {:?}", supervisor.status())
                }
            };
            assert_eq!(status.segments_completed, 3);
            assert_eq!(status.discontinuities, 0);
        }
        assert_eq!(store.stats().active_recorders, 0);
    }

    #[test]
    fn queued_finalization_cancels_without_waiting_for_the_busy_root_worker() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(12),
                max_active_recorders: 2,
            },
        )
        .expect("recording store");
        let path = RecordingPathPolicy::new(".flv", false).expect("path policy");
        let config = RecorderWorkerConfig {
            max_queue_messages: 8,
            max_queue_bytes: 1024,
            rotation_interval: Some(Duration::from_millis(1)),
            shutdown_timeout: Duration::from_millis(40),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        };
        let start = |name: &[u8]| {
            RecorderWorker::start(
                store.clone(),
                &path,
                name,
                1_721_619_000,
                RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
                config,
            )
            .expect("recorder worker")
        };
        let first = start(b"first");
        let second = start(b"second");
        for worker in [&first, &second] {
            enqueue(
                worker,
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
            );
            wait_until(|| worker.status().events_processed == 1);
        }
        let first_gate = first.install_publication_gate();

        enqueue(
            &first,
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
        );
        assert!(first_gate.wait_before_claim(Duration::from_secs(1)));
        enqueue(
            &second,
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x33"[..])).unwrap(),
        );
        wait_until(|| second.status().segments_started == 2);

        let shutdown = second.shutdown();
        let supervisor = match shutdown {
            RecorderShutdown::TimedOut(supervisor) => supervisor,
            RecorderShutdown::Joined(status) => {
                panic!("queued recorder unexpectedly joined before cancellation: {status:?}")
            }
        };
        wait_until(|| supervisor.is_finished());
        let status = supervisor.join();
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
        );
        assert_eq!(store.stats().active_recorders, 1);

        first_gate.allow_claim();
        assert!(first_gate.wait_after_claim(Duration::from_secs(1)));
        first_gate.allow_publication();
        assert!(matches!(first.shutdown(), RecorderShutdown::Joined(_)));
        assert_eq!(store.stats().active_recorders, 0);
        assert_eq!(
            fs::read_dir(root.path())
                .expect("recording entries")
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_file())
                .count(),
            4
        );
        assert_eq!(
            fs::read_dir(root.path())
                .expect("recording entries")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
                .count(),
            0
        );
    }

    #[test]
    fn third_unresolved_rotation_fails_explicitly_and_preserves_the_current_segment() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let worker = RecorderWorker::start(
            store,
            &RecordingPathPolicy::new(".flv", false).expect("path policy"),
            b"camera",
            1_721_619_000,
            RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
            RecorderWorkerConfig {
                max_queue_messages: 8,
                max_queue_bytes: 1024,
                rotation_interval: Some(Duration::from_millis(1)),
                shutdown_timeout: Duration::from_secs(1),
                video_codec: None,
                ..RecorderWorkerConfig::default()
            },
        )
        .expect("recorder worker");
        enqueue(
            &worker,
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
        );
        wait_until(|| worker.status().events_processed == 1);
        let gate = worker.install_publication_gate();

        for timestamp in 1..=3 {
            enqueue(
                &worker,
                MediaEvent::audio(
                    timestamp,
                    Arc::<[u8]>::from(
                        vec![
                            0xaf,
                            0x01,
                            u8::try_from(timestamp).expect("small test timestamp"),
                        ]
                        .into_boxed_slice(),
                    ),
                )
                .unwrap(),
            );
            if timestamp == 1 {
                assert!(gate.wait_before_claim(Duration::from_secs(1)));
            }
        }
        wait_until(|| {
            matches!(
                worker.status().phase,
                RecorderWorkerPhase::Failed(RecorderFailure::Finalize)
            )
        });
        let failed = worker.status();
        assert_eq!(failed.discontinuities, 0);
        let current = failed
            .recoverable_partial_name
            .expect("current segment remains recoverable");
        assert!(root.path().join(current).is_file());

        gate.allow_claim();
        assert!(gate.wait_after_claim(Duration::from_secs(1)));
        gate.allow_publication();
        let status = match worker.shutdown() {
            RecorderShutdown::Joined(status) => status,
            RecorderShutdown::TimedOut(supervisor) => {
                panic!("failed recorder did not join: {:?}", supervisor.status())
            }
        };
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::Finalize)
        );
    }

    #[test]
    fn record_mask_filters_events_before_the_worker_queue() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(4),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let worker = RecorderWorker::start(
            store,
            &RecordingPathPolicy::new(".flv", false).expect("path policy"),
            b"camera",
            1_721_619_000,
            RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
            RecorderWorkerConfig {
                record_mask: RecorderMediaMask::new(false, true, false),
                ..RecorderWorkerConfig::default()
            },
        )
        .expect("recorder worker");

        let audio =
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).expect("audio event");
        assert_eq!(worker.try_enqueue(audio), RecorderEnqueueResult::Filtered);
        assert_eq!(worker.status().queue_messages, 0);

        let video = MediaEvent::video(0, Arc::<[u8]>::from(&b"\x17\x01\x00\x00\x00\x01\x02"[..]))
            .expect("video event");
        assert_eq!(worker.try_enqueue(video), RecorderEnqueueResult::Queued);
        let _ = worker.shutdown();
    }

    #[test]
    fn invalid_recording_bounds_are_rejected_before_thread_start() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(4),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let path = RecordingPathPolicy::new(".flv", false).expect("path policy");
        let start_time = RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time");

        let invalid_mask = RecorderWorker::start(
            store.clone(),
            &path,
            b"mask",
            1_721_619_000,
            start_time,
            RecorderWorkerConfig {
                record_mask: RecorderMediaMask::new(false, false, false),
                ..RecorderWorkerConfig::default()
            },
        );
        assert!(matches!(
            invalid_mask,
            Err(RecorderWorkerStartError::InvalidRecordMask)
        ));

        let invalid_limit = RecorderWorker::start(
            store,
            &path,
            b"limit",
            1_721_619_000,
            start_time,
            RecorderWorkerConfig {
                max_frames: Some(0),
                ..RecorderWorkerConfig::default()
            },
        );
        assert!(matches!(
            invalid_limit,
            Err(RecorderWorkerStartError::InvalidRecordingLimit)
        ));
    }

    #[test]
    fn notifications_report_the_worker_lifecycle_when_enabled() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(4),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let worker = RecorderWorker::start(
            store,
            &RecordingPathPolicy::new(".flv", false).expect("path policy"),
            b"camera",
            1_721_619_000,
            RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
            RecorderWorkerConfig {
                notify: true,
                ..RecorderWorkerConfig::default()
            },
        )
        .expect("recorder worker");
        enqueue(
            &worker,
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
        );
        wait_until(|| worker.status().last_notification == Some(RecorderNotification::Started));

        let status = match worker.shutdown() {
            RecorderShutdown::Joined(status) => status,
            RecorderShutdown::TimedOut(supervisor) => {
                panic!("notifying recorder timed out: {:?}", supervisor.status())
            }
        };
        assert_eq!(
            status.last_notification,
            Some(RecorderNotification::Stopped)
        );
    }

    fn enqueue(worker: &RecorderWorker, event: MediaEvent) {
        assert_eq!(worker.try_enqueue(event), RecorderEnqueueResult::Queued);
    }

    fn enqueue_audio_burst(worker: &RecorderWorker, timestamp_ms: u32, count: u8) {
        for payload in 0..count {
            enqueue(
                worker,
                MediaEvent::audio(
                    timestamp_ms,
                    Arc::<[u8]>::from(vec![0xaf, 0x01, payload].into_boxed_slice()),
                )
                .unwrap(),
            );
        }
    }

    fn wait_until(predicate: impl Fn() -> bool) {
        for _ in 0..200 {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("recorder condition timeout");
    }
}
