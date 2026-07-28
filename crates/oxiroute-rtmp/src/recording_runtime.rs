use std::{
    fmt,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    MediaEvent, MediaEventKind, OperationId, RecorderEnqueueResult, RecorderErrorCode,
    RecorderFailure, RecorderId, RecorderWorker, RecorderWorkerConfig, RecorderWorkerPhase,
    RecorderWorkerStatus, RecorderWorkerSupervisor, RecordingDateTime, RecordingPathPolicy,
    RecordingStore, RtmpRegistry, SessionId, StreamId,
};

const REAPER_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRecorderStart {
    Continuous,
    Manual,
}

#[derive(Clone)]
pub struct RtmpRecorderPolicy {
    name: String,
    start: RtmpRecorderStart,
    store: RecordingStore,
    path: RecordingPathPolicy,
    worker: RecorderWorkerConfig,
}

impl RtmpRecorderPolicy {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        start: RtmpRecorderStart,
        store: RecordingStore,
        path: RecordingPathPolicy,
        worker: RecorderWorkerConfig,
    ) -> Self {
        Self {
            name: name.into(),
            start,
            store,
            path,
            worker,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn start(&self) -> RtmpRecorderStart {
        self.start
    }

    #[must_use]
    pub const fn store(&self) -> &RecordingStore {
        &self.store
    }

    #[must_use]
    pub const fn path_policy(&self) -> &RecordingPathPolicy {
        &self.path
    }

    #[must_use]
    pub const fn worker_config(&self) -> RecorderWorkerConfig {
        self.worker
    }
}

impl fmt::Debug for RtmpRecorderPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpRecorderPolicy")
            .field("name", &self.name)
            .field("start", &self.start)
            .field("path", &self.path)
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

pub(crate) struct RecorderController {
    policy: RtmpRecorderPolicy,
    stream_name: Arc<[u8]>,
    reaper: RecorderReaperHandle,
    state: Mutex<ControllerState>,
    last_observed_at_unix_ms: AtomicU64,
}

struct ControllerState {
    active: bool,
    bootstrap: RecorderBootstrap,
    stopping: bool,
    worker: Option<RecorderWorker>,
    last_status: Option<RecorderWorkerStatus>,
}

#[derive(Default)]
struct RecorderBootstrap {
    metadata: Option<MediaEvent>,
    aac: Option<MediaEvent>,
    avc: Option<MediaEvent>,
    keyframe: Option<MediaEvent>,
    unsupported_video: bool,
}

impl RecorderBootstrap {
    fn update(&mut self, event: &MediaEvent) {
        match event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(event.clone()),
            MediaEventKind::AacSequenceHeader => self.aac = Some(event.clone()),
            MediaEventKind::AvcSequenceHeader => {
                self.avc = Some(event.clone());
                self.keyframe = None;
                self.unsupported_video = false;
            }
            MediaEventKind::HevcSequenceHeader | MediaEventKind::Av1SequenceHeader => {
                self.avc = None;
                self.keyframe = None;
                self.unsupported_video = true;
            }
            MediaEventKind::VideoKeyframe if !self.unsupported_video => {
                self.keyframe = Some(event.clone());
            }
            MediaEventKind::Audio
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {}
        }
    }

    fn events(&self) -> impl Iterator<Item = MediaEvent> + '_ {
        self.metadata
            .iter()
            .chain(self.aac.iter())
            .chain(self.avc.iter())
            .chain(self.keyframe.iter())
            .cloned()
    }
}

#[derive(Clone)]
pub(crate) struct RecorderRuntimeStatus {
    pub status: Option<RecorderWorkerStatus>,
    pub stopping: bool,
    pub observed_at_unix_ms: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct RecorderCommandContext {
    pub stream_id: StreamId,
    pub publisher_session_id: SessionId,
    pub recorder_id: RecorderId,
    pub operation_id: OperationId,
    pub at_unix_ms: u64,
}

impl RecorderController {
    pub(crate) fn new(
        policy: RtmpRecorderPolicy,
        stream_name: Arc<[u8]>,
        reaper: RecorderReaperHandle,
        at_unix_ms: u64,
    ) -> Self {
        Self {
            policy,
            stream_name,
            reaper,
            state: Mutex::new(ControllerState {
                active: true,
                bootstrap: RecorderBootstrap::default(),
                stopping: false,
                worker: None,
                last_status: None,
            }),
            last_observed_at_unix_ms: AtomicU64::new(at_unix_ms),
        }
    }

    pub(crate) fn start(
        self: &Arc<Self>,
        context: RecorderCommandContext,
    ) -> Result<(), RecorderErrorCode> {
        self.observe_at(context.at_unix_ms);
        loop {
            let mut state = self.lock();
            if !state.active {
                return Err(RecorderErrorCode::StalePublisher);
            }
            if state.bootstrap.unsupported_video {
                return Err(RecorderErrorCode::UnsupportedCodec);
            }
            if let Some(worker) = state.worker.as_ref() {
                let status = worker.status();
                if !matches!(
                    status.phase,
                    RecorderWorkerPhase::Failed(_) | RecorderWorkerPhase::Stopped
                ) {
                    state.last_status = Some(status);
                    return Ok(());
                }
            }
            let Some(worker) = state.worker.take() else {
                let opened_at_unix_seconds = context.at_unix_ms / 1_000;
                let opened_at_utc = RecordingDateTime::from_unix_seconds(opened_at_unix_seconds)
                    .map_err(|_| RecorderErrorCode::OpenFailed)?;
                let worker = RecorderWorker::start(
                    self.policy.store.clone(),
                    &self.policy.path,
                    &self.stream_name,
                    opened_at_unix_seconds,
                    opened_at_utc,
                    self.policy.worker,
                )
                .map_err(|error| match error {
                    crate::RecorderWorkerStartError::UnsupportedVideoCodec(_) => {
                        RecorderErrorCode::UnsupportedCodec
                    }
                    crate::RecorderWorkerStartError::ThreadSpawn(_) => {
                        RecorderErrorCode::BackendUnavailable
                    }
                    crate::RecorderWorkerStartError::Path(_)
                    | crate::RecorderWorkerStartError::InvalidQueueLimits
                    | crate::RecorderWorkerStartError::InvalidRotationInterval
                    | crate::RecorderWorkerStartError::InvalidShutdownTimeout => {
                        RecorderErrorCode::OpenFailed
                    }
                })?;
                for event in state.bootstrap.events() {
                    if worker.try_enqueue_at(event, Instant::now(), context.at_unix_ms)
                        != RecorderEnqueueResult::Queued
                    {
                        return Err(RecorderErrorCode::BackendUnavailable);
                    }
                }
                state.last_status = Some(worker.status());
                state.stopping = false;
                state.worker = Some(worker);
                return Ok(());
            };
            drop(state);
            self.reaper
                .submit(worker, Arc::downgrade(self), None, context.at_unix_ms);
        }
    }

    pub(crate) fn stop(self: &Arc<Self>, context: RecorderCommandContext) -> bool {
        self.observe_at(context.at_unix_ms);
        let mut state = self.lock();
        state.stopping = true;
        let Some(worker) = state.worker.take() else {
            state.stopping = false;
            return false;
        };
        state.last_status = Some(worker.status());
        drop(state);
        self.reaper.submit(
            worker,
            Arc::downgrade(self),
            Some(ReapCompletion {
                registry: self.reaper.registry.clone(),
                context,
            }),
            context.at_unix_ms,
        );
        true
    }

    pub(crate) fn try_enqueue(&self, event: MediaEvent, at_unix_ms: u64) -> RecorderEnqueueResult {
        self.observe_at(at_unix_ms);
        let mut state = self.lock();
        state.bootstrap.update(&event);
        let Some(worker) = state.worker.as_ref() else {
            return RecorderEnqueueResult::Inactive;
        };
        let result = worker.try_enqueue_at(event, Instant::now(), at_unix_ms);
        state.last_status = Some(worker.status());
        result
    }

    pub(crate) fn status(&self) -> RecorderRuntimeStatus {
        let mut state = self.lock();
        if let Some(worker) = state.worker.as_ref() {
            state.last_status = Some(worker.status());
        }
        RecorderRuntimeStatus {
            status: state.last_status.clone(),
            stopping: state.stopping,
            observed_at_unix_ms: self.last_observed_at_unix_ms.load(Ordering::Acquire),
        }
    }

    pub(crate) fn deactivate(self: &Arc<Self>, at_unix_ms: u64) {
        self.observe_at(at_unix_ms);
        let mut state = self.lock();
        state.active = false;
        state.stopping = true;
        let Some(worker) = state.worker.take() else {
            state.stopping = false;
            return;
        };
        state.last_status = Some(worker.status());
        drop(state);
        self.reaper
            .submit(worker, Arc::downgrade(self), None, at_unix_ms);
    }

    fn finish_reap(&self, status: RecorderWorkerStatus, at_unix_ms: u64) {
        self.observe_at(at_unix_ms);
        let mut state = self.lock();
        state.last_status = Some(status);
        state.stopping = false;
    }

    fn observe_at(&self, at_unix_ms: u64) {
        self.last_observed_at_unix_ms
            .fetch_max(at_unix_ms, Ordering::AcqRel);
    }

    fn lock(&self) -> MutexGuard<'_, ControllerState> {
        self.state
            .lock()
            .expect("recorder controller mutex poisoned")
    }
}

#[derive(Clone)]
pub(crate) struct RecorderReaperHandle {
    queue: Arc<ReaperQueue>,
    registry: Weak<RtmpRegistry>,
}

pub(crate) struct RecorderReaper {
    queue: Arc<ReaperQueue>,
    thread: Option<JoinHandle<()>>,
}

pub(crate) struct RecorderReaperOwner {
    reaper: Mutex<Option<RecorderReaper>>,
}

struct ReapCompletion {
    registry: Weak<RtmpRegistry>,
    context: RecorderCommandContext,
}

struct ReaperQueue {
    sender: Sender<ReaperCommand>,
    capacity: usize,
    pending: Mutex<usize>,
    available: Condvar,
}

enum ReaperCommand {
    Reap(ReapTask),
    Shutdown,
}

struct ReapTask {
    supervisor: Option<RecorderWorkerSupervisor>,
    controller: Weak<RecorderController>,
    completion: Option<ReapCompletion>,
    deadline: Instant,
    completion_at_unix_ms: u64,
    cancelled: bool,
}

impl RecorderReaper {
    pub(crate) fn start(
        capacity: usize,
        registry: Weak<RtmpRegistry>,
    ) -> (Arc<RecorderReaperOwner>, RecorderReaperHandle) {
        let (sender, receiver) = mpsc::channel();
        let queue = Arc::new(ReaperQueue {
            sender,
            capacity: capacity.max(1),
            pending: Mutex::new(0),
            available: Condvar::new(),
        });
        let worker_queue = Arc::clone(&queue);
        let thread = thread::Builder::new()
            .name("rtmp-recorder-reaper".to_owned())
            .spawn(move || run_reaper(&receiver, &worker_queue))
            .expect("recorder reaper thread must start");
        (
            Arc::new(RecorderReaperOwner {
                reaper: Mutex::new(Some(Self {
                    queue: Arc::clone(&queue),
                    thread: Some(thread),
                })),
            }),
            RecorderReaperHandle { queue, registry },
        )
    }

    pub(crate) fn shutdown(mut self) {
        let _ = self.queue.sender.send(ReaperCommand::Shutdown);
        if self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
        {
            // The registry is already shutting down; no caller can consume a reaper failure.
        }
    }
}

impl Drop for RecorderReaperOwner {
    fn drop(&mut self) {
        if let Some(reaper) = self
            .reaper
            .get_mut()
            .expect("recorder reaper owner mutex poisoned")
            .take()
        {
            reaper.shutdown();
        }
    }
}

impl RecorderReaperHandle {
    fn submit(
        &self,
        worker: RecorderWorker,
        controller: Weak<RecorderController>,
        completion: Option<ReapCompletion>,
        completion_at_unix_ms: u64,
    ) {
        let supervisor = worker.into_supervisor();
        let deadline = Instant::now() + supervisor.shutdown_timeout();
        self.queue.submit(ReapTask {
            supervisor: Some(supervisor),
            controller,
            completion,
            deadline,
            completion_at_unix_ms,
            cancelled: false,
        });
    }
}

impl ReaperQueue {
    fn submit(&self, task: ReapTask) {
        let mut pending = self
            .pending
            .lock()
            .expect("recorder reaper pending mutex poisoned");
        pending = self
            .available
            .wait_while(pending, |pending| *pending >= self.capacity)
            .expect("recorder reaper pending mutex poisoned");
        *pending += 1;
        drop(pending);

        if self.sender.send(ReaperCommand::Reap(task)).is_err() {
            self.complete();
            panic!("recorder reaper thread disconnected");
        }
    }

    fn complete(&self) {
        let mut pending = self
            .pending
            .lock()
            .expect("recorder reaper pending mutex poisoned");
        *pending = pending
            .checked_sub(1)
            .expect("recorder reaper completed an untracked task");
        drop(pending);
        self.available.notify_one();
    }
}

fn run_reaper(receiver: &Receiver<ReaperCommand>, queue: &ReaperQueue) {
    let mut tasks = Vec::new();
    let mut shutting_down = false;
    loop {
        match receiver.recv_timeout(REAPER_POLL_INTERVAL) {
            Ok(ReaperCommand::Reap(task)) => tasks.push(task),
            Ok(ReaperCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                shutting_down = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        loop {
            match receiver.try_recv() {
                Ok(ReaperCommand::Reap(task)) => tasks.push(task),
                Ok(ReaperCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                    shutting_down = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        let now = Instant::now();
        for task in &mut tasks {
            let supervisor = task
                .supervisor
                .as_ref()
                .expect("unfinished reaper task owns a supervisor");
            if (shutting_down || now >= task.deadline) && !task.cancelled {
                supervisor.cancel();
                task.cancelled = true;
            }
        }
        let mut index = 0;
        while index < tasks.len() {
            let finished = tasks[index]
                .supervisor
                .as_ref()
                .is_some_and(RecorderWorkerSupervisor::is_finished);
            if !finished {
                index += 1;
                continue;
            }
            let mut task = tasks.swap_remove(index);
            let status = task
                .supervisor
                .take()
                .expect("finished reaper task owns a supervisor")
                .join();
            if let Some(controller) = task.controller.upgrade() {
                controller.finish_reap(status.clone(), task.completion_at_unix_ms);
            }
            if let Some(completion) = task.completion {
                if let Some(registry) = completion.registry.upgrade() {
                    registry.complete_worker_stop(completion.context, &status);
                }
            }
            queue.complete();
        }
        if shutting_down && tasks.is_empty() {
            return;
        }
    }
}

pub(crate) fn recorder_error_code(failure: RecorderFailure) -> RecorderErrorCode {
    match failure {
        RecorderFailure::Open => RecorderErrorCode::OpenFailed,
        RecorderFailure::Write => RecorderErrorCode::WriteFailed,
        RecorderFailure::Finalize => RecorderErrorCode::CloseFailed,
        RecorderFailure::FileSync => RecorderErrorCode::FileSyncFailed,
        RecorderFailure::Publish => RecorderErrorCode::PublishFailed,
        RecorderFailure::DirectorySync => RecorderErrorCode::DirectorySyncFailed,
        RecorderFailure::Discontinuity => RecorderErrorCode::QueueDiscontinuity,
        RecorderFailure::UnsupportedCodec => RecorderErrorCode::UnsupportedCodec,
        RecorderFailure::ShutdownTimedOut => RecorderErrorCode::ShutdownTimedOut,
        RecorderFailure::WorkerPanicked => RecorderErrorCode::WorkerPanicked,
    }
}
