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
    FlvMuxer, MediaEvent, MediaEventKind, RecordingCommit, RecordingDateTime, RecordingFile,
    RecordingPathError, RecordingPathPolicy, RecordingStore, RecordingStoreError,
    RecordingTimeBasis, recording_store::RecordingCommitCancellation,
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
pub enum RecorderWorkerPhase {
    Starting,
    Recording,
    Stopped,
    Failed(RecorderFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderEnqueueResult {
    Queued,
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
    #[error("recorder shutdown timeout must be nonzero")]
    InvalidShutdownTimeout,
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
    current_partial_exists: Mutex<Option<Arc<AtomicBool>>>,
    #[cfg(test)]
    panic_on_finish: AtomicBool,
    #[cfg(test)]
    panic_after_finish: AtomicBool,
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
        let path_policy = path_policy.clone();
        let stream_name = Arc::<[u8]>::from(stream_name);
        let setup = WorkerSetup {
            store,
            path_policy,
            stream_name,
            rotation_interval: rotation_interval_ms
                .map(|value| Duration::from_millis(value.into())),
        };
        let shared = Arc::new(WorkerShared {
            max_queue_messages: config.max_queue_messages,
            max_queue_bytes: config.max_queue_bytes,
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
            current_partial_exists: Mutex::new(None),
            #[cfg(test)]
            panic_on_finish: AtomicBool::new(false),
            #[cfg(test)]
            panic_after_finish: AtomicBool::new(false),
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
        let commit = self
            .commit_cancellation
            .lock()
            .expect("recorder commit cancellation mutex poisoned")
            .clone();
        if let Some(commit) = commit {
            return commit.cancel();
        }
        !matches!(
            self.lock_status().phase,
            RecorderWorkerPhase::Stopped | RecorderWorkerPhase::Failed(_)
        )
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
    segment_started_at: Option<Instant>,
    segment_started_at_unix_seconds: Option<u64>,
    latest_event_at_unix_seconds: u64,
    last_written_at_unix_seconds: u64,
    video_seen: bool,
    segment_headers: SegmentHeaders,
}

struct WorkerSetup {
    store: RecordingStore,
    path_policy: RecordingPathPolicy,
    stream_name: Arc<[u8]>,
    rotation_interval: Option<Duration>,
}

fn run_worker(shared: &Arc<WorkerShared>, setup: WorkerSetup) {
    {
        let mut status = shared.lock_status();
        if !matches!(status.phase, RecorderWorkerPhase::Failed(_)) {
            status.phase = RecorderWorkerPhase::Recording;
        }
    }
    let WorkerSetup {
        store,
        path_policy,
        stream_name,
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
        segment_started_at: None,
        segment_started_at_unix_seconds: None,
        latest_event_at_unix_seconds: 0,
        last_written_at_unix_seconds: 0,
        video_seen: false,
        segment_headers: SegmentHeaders::default(),
    };

    loop {
        match shared.next_event() {
            NextEvent::Event(event) => {
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
        shared.fail(failure);
        return;
    }
    shared.stop_successfully();
}

impl WorkerContext {
    fn process(&mut self, queued: &QueuedMediaEvent) -> Result<(), WorkerError> {
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
            self.finish_segment()?;
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
        let (segment_name, segment, segment_started_at, segment_started_at_unix_seconds) =
            if let Some((segment_name, started_at_unix_seconds)) = resume {
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
                )
            } else {
                let segment_name = self
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
                    &segment_name,
                    &headers,
                    Arc::clone(&self.shared.bytes_written),
                    &self.shared,
                )?;
                (segment_name, segment, arrived_at, arrived_at_unix_seconds)
            };
        let next_segment_sequence = self
            .next_segment_sequence
            .checked_add(1)
            .ok_or_else(|| WorkerError::new(RecorderFailure::Open))?;
        if self.shared.discontinuity_requested.load(Ordering::Acquire) {
            return Err(WorkerError::new(RecorderFailure::Discontinuity));
        }
        if self.shared.cancelled.load(Ordering::Acquire) {
            return Err(WorkerError::new(RecorderFailure::ShutdownTimedOut));
        }
        self.segment = Some(segment);
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
    ) -> Result<Option<(String, u64)>, WorkerError> {
        let Some(interval) = self.rotation_interval else {
            return Ok(None);
        };
        let names = self
            .store
            .recording_names()
            .map_err(|_| WorkerError::new(RecorderFailure::Open))?;
        Ok(names
            .into_iter()
            .filter_map(|name| {
                let started_at = self
                    .path_policy
                    .segment_start_from_filename(&self.stream_name, &name)?;
                let age = arrived_at_unix_seconds.checked_sub(started_at)?;
                (Duration::from_secs(age) < interval).then_some((name, started_at))
            })
            .max_by_key(|(_, started_at)| *started_at))
    }

    fn finish_segment(&mut self) -> Result<(), WorkerError> {
        let Some(segment) = self.segment.as_ref() else {
            return Ok(());
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
        let final_name = self
            .path_policy
            .segment_filename(
                &self.stream_name,
                naming_time,
                RecordingDateTime::from_unix_seconds(naming_time)
                    .map_err(|_| WorkerError::new(RecorderFailure::Finalize))?,
                sequence,
            )
            .map_err(|_| WorkerError::new(RecorderFailure::Finalize))?;
        let segment = self
            .segment
            .take()
            .expect("segment remains owned through final-name rendering");
        let RecordingCommit { relative_name, .. } = segment.finish(final_name)?;
        #[cfg(test)]
        assert!(
            !self.shared.panic_after_finish.swap(false, Ordering::AcqRel),
            "injected recorder post-finish panic"
        );
        *self
            .shared
            .current_partial_exists
            .lock()
            .expect("recorder partial existence mutex poisoned") = None;
        let mut status = self.shared.lock_status();
        status.current_relative_name = None;
        status.current_partial_name = None;
        status.last_completed_relative_name = Some(relative_name);
        status.segments_completed = status.segments_completed.saturating_add(1);
        Ok(())
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

struct Segment {
    muxer: FlvMuxer<CountedRecordingFile>,
    partial_relative_name: String,
    preserve_partial: Arc<AtomicBool>,
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
            .create_unless(relative_name, || {
                shared.cancelled.load(Ordering::Acquire)
                    || shared.discontinuity_requested.load(Ordering::Acquire)
            })
            .map_err(|error| {
                if matches!(error, RecordingStoreError::CreationCancelled) {
                    if shared.discontinuity_requested.load(Ordering::Acquire) {
                        WorkerError::new(RecorderFailure::Discontinuity)
                    } else {
                        WorkerError::new(RecorderFailure::ShutdownTimedOut)
                    }
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
            .resume(relative_name)
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
        })
    }

    fn write(&mut self, event: &MediaEvent) -> Result<(), WorkerError> {
        write_to_muxer(&mut self.muxer, event)
    }

    fn preserve(&self) {
        self.preserve_partial.store(true, Ordering::Release);
    }

    fn finish(self, final_name: String) -> Result<RecordingCommit, WorkerError> {
        self.preserve_partial.store(true, Ordering::Release);
        let mut file = self.muxer.close().map_err(|_| WorkerError {
            kind: RecorderFailure::Finalize,
            recoverable_partial_name: Some(self.partial_relative_name),
            published_but_not_durable_relative_name: None,
        })?;
        file.inner
            .set_final_relative_name(final_name)
            .map_err(|error| WorkerError::finalization(&error))?;
        file.commit()
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
