use std::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod policy;

pub use policy::{RtmpRecorderPolicy, RtmpRecorderStart};

use crate::{
    MediaEvent, MediaEventKind, OperationId, RecorderEnqueueResult, RecorderErrorCode,
    RecorderFailure, RecorderId, RecorderWorker, RecorderWorkerConfig, RecorderWorkerPhase,
    RecorderWorkerStatus, RecorderWorkerSupervisor, RecordingDateTime, RtmpRegistry, SessionId,
    StreamId,
};

const REAPER_POLL_INTERVAL: Duration = Duration::from_millis(2);
#[cfg(test)]
const CONTINUOUS_RESTART_DELAY: Duration = Duration::from_millis(10);
#[cfg(not(test))]
const CONTINUOUS_RESTART_DELAY: Duration = Duration::from_millis(250);

pub(crate) struct RecorderController {
    policy: RtmpRecorderPolicy,
    stream_name: Arc<[u8]>,
    reaper: RecorderReaperHandle,
    state: Mutex<ControllerState>,
    state_changed: Condvar,
    last_observed_at_unix_ms: AtomicU64,
}

struct ControllerState {
    active: bool,
    bootstrap: RecorderBootstrap,
    stopping: bool,
    worker: Option<RecorderWorker>,
    worker_generation: u64,
    last_status: Option<RecorderWorkerStatus>,
    completed_status: Option<RecorderWorkerStatus>,
    restart_context: Option<RecorderCommandContext>,
    restart_after: Instant,
    recovering: bool,
    recovery_events_dropped: u64,
}

#[derive(Default)]
struct RecorderBootstrap {
    metadata: Option<MediaEvent>,
    aac: Option<MediaEvent>,
    video: Option<MediaEvent>,
    keyframe: Option<MediaEvent>,
}

struct BootstrapReplay {
    events: Vec<MediaEvent>,
    reserved_event_fits: bool,
}

impl RecorderBootstrap {
    fn update(&mut self, event: &MediaEvent) {
        match event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(event.clone()),
            MediaEventKind::AacSequenceHeader => self.aac = Some(event.clone()),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {
                self.video = Some(event.clone());
                self.keyframe = None;
            }
            MediaEventKind::VideoKeyframe => {
                self.keyframe = Some(event.clone());
            }
            MediaEventKind::Audio
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {}
        }
    }

    fn events(&self) -> impl Iterator<Item = MediaEvent> + '_ {
        self.metadata
            .iter()
            .chain(self.aac.iter())
            .chain(self.video.iter())
            .chain(self.keyframe.iter())
            .cloned()
    }

    fn replay_events(
        &self,
        config: RecorderWorkerConfig,
        reserved_event: Option<&MediaEvent>,
    ) -> Option<BootstrapReplay> {
        let events: Vec<_> = self.events().collect();
        let replay_fits = media_events_fit(&events, config);
        let reserved_event_fits = replay_fits
            && reserved_event.is_none_or(|reserved| {
                events
                    .len()
                    .checked_add(1)
                    .is_some_and(|messages| messages <= config.max_queue_messages)
                    && events
                        .iter()
                        .try_fold(reserved.payload_len(), |bytes, event| {
                            bytes.checked_add(event.payload_len())
                        })
                        .is_some_and(|bytes| bytes <= config.max_queue_bytes)
            });
        if replay_fits {
            return Some(BootstrapReplay {
                events,
                reserved_event_fits,
            });
        }
        None
    }

    fn mark_recovery_gap(&mut self, kind: MediaEventKind, result: RecorderEnqueueResult) {
        if kind != MediaEventKind::VideoKeyframe
            || matches!(
                result,
                RecorderEnqueueResult::Queued | RecorderEnqueueResult::Filtered
            )
        {
            self.keyframe = None;
        }
    }
}

#[derive(Clone)]
pub(crate) struct RecorderRuntimeStatus {
    pub status: Option<RecorderWorkerStatus>,
    pub recovery_events_dropped: u64,
    pub stopping: bool,
    pub recovering: bool,
    pub worker_generation: u64,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecorderStartFailure {
    RetryableCapacity,
    Failed(RecorderErrorCode),
}

enum StartNewWorkerFailure {
    WaitForReap,
    Failed(RecorderStartFailure),
}

impl From<RecorderStartFailure> for StartNewWorkerFailure {
    fn from(failure: RecorderStartFailure) -> Self {
        Self::Failed(failure)
    }
}

impl RecorderStartFailure {
    const fn code(self) -> RecorderErrorCode {
        match self {
            Self::RetryableCapacity => RecorderErrorCode::OpenFailed,
            Self::Failed(code) => code,
        }
    }
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
                worker_generation: 0,
                last_status: None,
                completed_status: None,
                restart_context: None,
                restart_after: Instant::now(),
                recovering: false,
                recovery_events_dropped: 0,
            }),
            state_changed: Condvar::new(),
            last_observed_at_unix_ms: AtomicU64::new(at_unix_ms),
        }
    }

    pub(crate) fn start(
        self: &Arc<Self>,
        context: RecorderCommandContext,
    ) -> Result<(), RecorderErrorCode> {
        self.start_with_reservation(context, None)
            .map(|_| ())
            .map_err(RecorderStartFailure::code)
    }

    pub(crate) fn start_continuous(
        self: &Arc<Self>,
        context: RecorderCommandContext,
    ) -> Result<(), RecorderStartFailure> {
        self.start_with_reservation(context, None).map(|_| ())
    }

    fn start_with_reservation(
        self: &Arc<Self>,
        context: RecorderCommandContext,
        reserved_event: Option<&MediaEvent>,
    ) -> Result<bool, RecorderStartFailure> {
        self.observe_at(context.at_unix_ms);
        loop {
            let mut state = self.lock();
            if !state.active {
                return Err(RecorderStartFailure::Failed(
                    RecorderErrorCode::StalePublisher,
                ));
            }
            if self.policy.start() == RtmpRecorderStart::Continuous {
                state.restart_context = Some(context);
            }
            if let Some(worker) = state.worker.as_ref() {
                let status = worker.status();
                if !matches!(
                    status.phase,
                    RecorderWorkerPhase::Failed(_) | RecorderWorkerPhase::Stopped
                ) {
                    state.last_status = Some(accumulate_worker_status(
                        self.completed_status(&state),
                        status,
                    ));
                    return Ok(true);
                }
            }
            let Some(worker) = state.worker.take() else {
                match self.start_new_worker(state, context, reserved_event) {
                    Ok(started) => return Ok(started),
                    Err(StartNewWorkerFailure::WaitForReap) => continue,
                    Err(StartNewWorkerFailure::Failed(failure)) => return Err(failure),
                }
            };
            let worker_generation = state.worker_generation;
            state.last_status = Some(accumulate_worker_status(
                self.completed_status(&state),
                worker.status(),
            ));
            state.stopping = true;
            drop(state);
            self.reaper.submit(
                worker,
                worker_generation,
                Arc::downgrade(self),
                None,
                context.at_unix_ms,
            );
        }
    }

    fn start_new_worker(
        self: &Arc<Self>,
        mut state: MutexGuard<'_, ControllerState>,
        context: RecorderCommandContext,
        reserved_event: Option<&MediaEvent>,
    ) -> Result<bool, StartNewWorkerFailure> {
        let admission = self
            .reaper
            .queue
            .admission()
            .ok_or(RecorderStartFailure::Failed(
                RecorderErrorCode::BackendUnavailable,
            ))?;
        let worker_generation =
            state
                .worker_generation
                .checked_add(1)
                .ok_or(RecorderStartFailure::Failed(
                    RecorderErrorCode::BackendUnavailable,
                ))?;
        let opened_at_unix_seconds = context.at_unix_ms / 1_000;
        let opened_at_utc = RecordingDateTime::from_unix_seconds(opened_at_unix_seconds)
            .map_err(|_| RecorderStartFailure::Failed(RecorderErrorCode::OpenFailed))?;
        let mut bootstrap = state
            .bootstrap
            .replay_events(self.policy.worker_config(), reserved_event)
            .ok_or(RecorderStartFailure::Failed(
                RecorderErrorCode::QueueDiscontinuity,
            ))?;
        let worker = match RecorderWorker::start(
            self.policy.store().clone(),
            self.policy.path_policy(),
            &self.stream_name,
            opened_at_unix_seconds,
            opened_at_utc,
            self.policy.worker_config(),
        ) {
            Ok(worker) => worker,
            Err(crate::RecorderWorkerStartError::Capacity(
                crate::RecordingStoreError::ActiveRecorderLimit { .. },
            )) if state.stopping => {
                drop(admission);
                drop(
                    self.state_changed
                        .wait_while(state, |state| state.active && state.stopping)
                        .expect("recorder controller mutex poisoned while waiting for reap"),
                );
                return Err(StartNewWorkerFailure::WaitForReap);
            }
            Err(crate::RecorderWorkerStartError::Capacity(
                crate::RecordingStoreError::ActiveRecorderLimit { .. },
            )) => {
                return Err(StartNewWorkerFailure::Failed(
                    RecorderStartFailure::RetryableCapacity,
                ));
            }
            Err(error) => {
                return Err(StartNewWorkerFailure::Failed(RecorderStartFailure::Failed(
                    recorder_start_error_code(&error),
                )));
            }
        };
        enqueue_bootstrap_replay(
            &mut state.bootstrap,
            &worker,
            &mut bootstrap,
            context.at_unix_ms,
        );
        state.last_status = Some(accumulate_worker_status(
            self.completed_status(&state),
            worker.status(),
        ));
        state.stopping = false;
        state.recovering = false;
        state.restart_after = Instant::now();
        state.worker_generation = worker_generation;
        state.worker = Some(worker);
        Ok(bootstrap.reserved_event_fits)
    }

    pub(crate) fn stop(self: &Arc<Self>, context: RecorderCommandContext) -> bool {
        self.observe_at(context.at_unix_ms);
        let mut state = self.lock();
        state.restart_context = None;
        state.recovering = false;
        let already_stopping = state.stopping;
        state.stopping = true;
        let Some(worker) = state.worker.take() else {
            state.stopping = already_stopping;
            return false;
        };
        let worker_generation = state.worker_generation;
        state.last_status = Some(accumulate_worker_status(
            self.completed_status(&state),
            worker.status(),
        ));
        drop(state);
        self.reaper.submit(
            worker,
            worker_generation,
            Arc::downgrade(self),
            Some(ReapCompletion {
                registry: self.reaper.registry.clone(),
                context,
            }),
            context.at_unix_ms,
        );
        true
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn try_enqueue(
        self: &Arc<Self>,
        event: MediaEvent,
        at_unix_ms: u64,
    ) -> RecorderEnqueueResult {
        self.observe_at(at_unix_ms);
        if !self
            .policy
            .worker_config()
            .record_mask
            .accepts(event.kind())
        {
            return RecorderEnqueueResult::Filtered;
        }
        loop {
            let mut state = self.lock();
            if state.worker.is_none() {
                let kind = event.kind();
                let replayed_event = matches!(
                    kind,
                    MediaEventKind::Metadata
                        | MediaEventKind::AacSequenceHeader
                        | MediaEventKind::AvcSequenceHeader
                        | MediaEventKind::VideoKeyframe
                );
                if replayed_event {
                    state.bootstrap.update(&event);
                }
                let restart = state
                    .restart_context
                    .filter(|_| {
                        state.active && !state.stopping && Instant::now() >= state.restart_after
                    })
                    .map(|mut context| {
                        context.at_unix_ms = at_unix_ms;
                        context
                    });
                if let Some(context) = restart {
                    drop(state);
                    let reserved_event = (!replayed_event).then_some(&event);
                    match self.start_with_reservation(context, reserved_event) {
                        Ok(true) if replayed_event => return RecorderEnqueueResult::Queued,
                        Ok(true) => continue,
                        Ok(false) => {
                            state = self.lock();
                            record_recovery_drop(&mut state, &event);
                            return RecorderEnqueueResult::Inactive;
                        }
                        Err(_) => {}
                    }
                    state = self.lock();
                    state.recovering = true;
                    state.restart_after = Instant::now() + CONTINUOUS_RESTART_DELAY;
                }
                record_recovery_drop(&mut state, &event);
                return RecorderEnqueueResult::Inactive;
            }
            let kind = event.kind();
            state.bootstrap.update(&event);
            let worker = state
                .worker
                .as_ref()
                .expect("checked recorder worker remains controller-owned");
            let result = worker.try_enqueue_at(event, Instant::now(), at_unix_ms);
            let status = worker.status();
            let recover = self.policy.start() == RtmpRecorderStart::Continuous
                && matches!(status.phase, RecorderWorkerPhase::Failed(_));
            if result != RecorderEnqueueResult::Queued || recover {
                state.bootstrap.mark_recovery_gap(kind, result);
            }
            state.last_status = Some(accumulate_worker_status(
                self.completed_status(&state),
                status,
            ));
            if !recover {
                if self.policy.start() == RtmpRecorderStart::Continuous
                    && result == RecorderEnqueueResult::Inactive
                {
                    state.recovering = true;
                    state.recovery_events_dropped = state.recovery_events_dropped.saturating_add(1);
                }
                return result;
            }
            if Instant::now() < state.restart_after {
                count_inactive_recovery_drop(&mut state, result);
                return result;
            }
            let worker = state
                .worker
                .take()
                .expect("failed recorder worker remains controller-owned");
            let worker_generation = state.worker_generation;
            if let Err(worker) =
                self.reaper
                    .try_submit(worker, worker_generation, Arc::downgrade(self), at_unix_ms)
            {
                state.worker = Some(worker);
                state.recovering = true;
                state.restart_after = Instant::now() + CONTINUOUS_RESTART_DELAY;
                count_inactive_recovery_drop(&mut state, result);
                return result;
            }
            state.stopping = true;
            state.recovering = true;
            state.restart_after = Instant::now() + CONTINUOUS_RESTART_DELAY;
            count_inactive_recovery_drop(&mut state, result);
            if let Some(context) = state.restart_context.as_mut() {
                context.at_unix_ms = at_unix_ms;
            }
            return result;
        }
    }

    pub(crate) fn status(&self) -> RecorderRuntimeStatus {
        let mut state = self.lock();
        if let Some(worker) = state.worker.as_ref() {
            state.last_status = Some(accumulate_worker_status(
                self.completed_status(&state),
                worker.status(),
            ));
        }
        RecorderRuntimeStatus {
            status: state.last_status.clone().map(|mut status| {
                status.events_dropped = status
                    .events_dropped
                    .saturating_add(state.recovery_events_dropped);
                status
            }),
            recovery_events_dropped: state.recovery_events_dropped,
            stopping: state.stopping,
            recovering: state.recovering,
            worker_generation: state.worker_generation,
            observed_at_unix_ms: self.last_observed_at_unix_ms.load(Ordering::Acquire),
        }
    }

    pub(crate) fn deactivate(self: &Arc<Self>, at_unix_ms: u64) {
        self.observe_at(at_unix_ms);
        let mut state = self.lock();
        state.active = false;
        state.stopping = true;
        state.restart_context = None;
        state.recovering = false;
        self.state_changed.notify_all();
        let Some(worker) = state.worker.take() else {
            state.stopping = false;
            return;
        };
        let worker_generation = state.worker_generation;
        state.last_status = Some(accumulate_worker_status(
            self.completed_status(&state),
            worker.status(),
        ));
        drop(state);
        self.reaper.submit(
            worker,
            worker_generation,
            Arc::downgrade(self),
            None,
            at_unix_ms,
        );
    }

    pub(crate) fn uses_reaper(&self, reaper: &RecorderReaperHandle) -> bool {
        Arc::ptr_eq(&self.reaper.queue, &reaper.queue)
    }

    pub(crate) fn observed_at_unix_ms(&self) -> u64 {
        self.last_observed_at_unix_ms.load(Ordering::Acquire)
    }

    fn finish_reap(
        self: &Arc<Self>,
        worker_generation: u64,
        status: RecorderWorkerStatus,
        at_unix_ms: u64,
    ) {
        self.observe_at(at_unix_ms);
        let mut state = self.lock();
        if state.worker_generation != worker_generation {
            return;
        }
        let completed = accumulate_worker_status(self.completed_status(&state), status);
        state.last_status = Some(completed.clone());
        if self.policy.start() == RtmpRecorderStart::Continuous {
            state.completed_status = Some(completed);
        }
        state.stopping = false;
        state.recovering = state.restart_context.is_some() && state.active;
        drop(state);
        self.state_changed.notify_all();
    }

    fn observe_at(&self, at_unix_ms: u64) {
        self.last_observed_at_unix_ms
            .fetch_max(at_unix_ms, Ordering::AcqRel);
    }

    fn completed_status<'a>(&self, state: &'a ControllerState) -> Option<&'a RecorderWorkerStatus> {
        if self.policy.start() == RtmpRecorderStart::Continuous {
            state.completed_status.as_ref()
        } else {
            None
        }
    }

    fn lock(&self) -> MutexGuard<'_, ControllerState> {
        self.state
            .lock()
            .expect("recorder controller mutex poisoned")
    }

    #[cfg(test)]
    fn install_publication_gate(&self) -> Arc<crate::recording_store::RecordingPublicationGate> {
        self.lock()
            .worker
            .as_ref()
            .expect("active recorder controller owns a worker")
            .install_publication_gate()
    }

    #[cfg(test)]
    fn panic_on_finish(&self) {
        self.lock()
            .worker
            .as_ref()
            .expect("active recorder controller owns a worker")
            .panic_on_finish();
    }

    #[cfg(test)]
    fn panic_after_finish(&self) {
        self.lock()
            .worker
            .as_ref()
            .expect("active recorder controller owns a worker")
            .panic_after_finish();
    }

    #[cfg(test)]
    pub(crate) fn fail_before_process(&self) {
        self.lock()
            .worker
            .as_ref()
            .expect("active recorder controller owns a worker")
            .fail_before_process();
    }
}

#[derive(Clone)]
pub(crate) struct RecorderReaperHandle {
    queue: Arc<ReaperQueue>,
    registry: Weak<RtmpRegistry>,
    cleanup: RecorderCleanupHandle,
}

#[derive(Clone)]
pub(crate) struct RecorderShutdownControl {
    queue: Arc<ReaperQueue>,
    cleanup: RecorderCleanupHandle,
}

pub(crate) struct RecorderReaper {
    queue: Arc<ReaperQueue>,
    thread: Option<JoinHandle<()>>,
    cleanup: RecorderCleanupHandle,
}

pub(crate) struct RecorderReaperOwner {
    reaper: Mutex<Option<RecorderReaper>>,
    shutdown: RtmpRecorderShutdown,
}

type RecorderRetirement = Box<dyn FnOnce() + Send + 'static>;

/// Completion handle for one RTMP recorder/reaper shutdown lifecycle.
#[derive(Clone)]
pub struct RtmpRecorderShutdown {
    queue: Arc<ReaperQueue>,
    retirement: Arc<Mutex<RecorderRetirementState>>,
}

#[derive(Clone)]
struct RecorderCleanupHandle {
    sender: Sender<CleanupCommand>,
}

enum CleanupCommand {
    Task {
        task: ReapTask,
        queue: Option<Arc<ReaperQueue>>,
    },
    Reaper {
        thread: JoinHandle<()>,
        queue: Arc<ReaperQueue>,
    },
    Wake,
}

struct ReapCompletion {
    registry: Weak<RtmpRegistry>,
    context: RecorderCommandContext,
}

struct ReaperQueue {
    sender: Sender<ReaperCommand>,
    capacity: usize,
    retirement: Arc<Mutex<RecorderRetirementState>>,
    state: Mutex<ReaperQueueState>,
    available: Condvar,
}

struct RecorderRetirementState {
    callback: Option<RecorderRetirement>,
    ran: bool,
}

struct ReaperQueueState {
    accepting: bool,
    failed: bool,
    pending: usize,
    shutdown_deadline: Option<Instant>,
    reaper_finished: bool,
}

enum ReaperCommand {
    Reap(ReapTask),
    Shutdown {
        deadline: Option<Instant>,
    },
    #[cfg(test)]
    Panic,
}

struct ReapTask {
    supervisor: Option<RecorderWorkerSupervisor>,
    worker_generation: u64,
    controller: Weak<RecorderController>,
    completion: Option<ReapCompletion>,
    deadline: Instant,
    completion_at_unix_ms: u64,
    cancelled: bool,
}

enum ReaperSubmitError {
    Closed(Box<ReapTask>),
    Failed(Box<ReapTask>),
}

impl RecorderReaper {
    pub(crate) fn start(
        capacity: usize,
        registry: Weak<RtmpRegistry>,
    ) -> (Arc<RecorderReaperOwner>, RecorderReaperHandle) {
        let cleanup = RecorderCleanupHandle::start();
        let (sender, receiver) = mpsc::channel();
        let queue = Arc::new(ReaperQueue {
            sender,
            capacity: capacity.max(1),
            retirement: Arc::new(Mutex::new(RecorderRetirementState {
                callback: None,
                ran: false,
            })),
            state: Mutex::new(ReaperQueueState {
                accepting: true,
                failed: false,
                pending: 0,
                shutdown_deadline: None,
                reaper_finished: false,
            }),
            available: Condvar::new(),
        });
        let worker_queue = Arc::clone(&queue);
        let worker_cleanup = cleanup.clone();
        let thread = thread::Builder::new()
            .name("rtmp-recorder-reaper".to_owned())
            .spawn(move || supervise_reaper(&receiver, &worker_queue, &worker_cleanup))
            .expect("recorder reaper thread must start");
        (
            Arc::new(RecorderReaperOwner {
                reaper: Mutex::new(Some(Self {
                    queue: Arc::clone(&queue),
                    thread: Some(thread),
                    cleanup: cleanup.clone(),
                })),
                shutdown: RtmpRecorderShutdown {
                    queue: Arc::clone(&queue),
                    retirement: Arc::clone(&queue.retirement),
                },
            }),
            RecorderReaperHandle {
                queue,
                registry,
                cleanup,
            },
        )
    }

    fn initiate_shutdown(mut self, deadline: Option<Instant>) {
        self.queue.shutdown(deadline);
        if let Some(thread) = self.thread.take() {
            self.cleanup.submit_reaper(thread, &self.queue);
        }
    }
}

impl RecorderReaperOwner {
    pub(crate) fn shutdown_handle(&self) -> RtmpRecorderShutdown {
        self.shutdown.clone()
    }

    pub(crate) fn initiate_shutdown(&self, deadline: Instant) -> RtmpRecorderShutdown {
        let reaper = self
            .reaper
            .lock()
            .expect("recorder reaper owner mutex poisoned")
            .take();
        if let Some(reaper) = reaper {
            reaper.initiate_shutdown(Some(deadline));
        } else {
            self.shutdown.queue.shutdown(Some(deadline));
        }
        self.shutdown.clone()
    }
}

impl RtmpRecorderShutdown {
    /// Returns true when both handles observe the same recorder shutdown lifecycle.
    #[must_use]
    pub fn is_same_lifecycle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.queue, &other.queue)
    }

    /// Returns true once the reaper has exited and every tracked worker has been reconciled.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.queue.run_retirement_if_complete();
        self.queue.shutdown_complete()
    }

    /// Waits until completion or the supplied absolute deadline.
    #[must_use]
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let complete = self.queue.wait_until_shutdown_complete(deadline);
        self.queue.run_retirement_if_complete();
        complete
    }

    pub(crate) fn set_retirement(&self, retire: impl FnOnce() + Send + 'static) {
        let mut retirement = self
            .retirement
            .lock()
            .expect("recorder retirement mutex poisoned");
        if !retirement.ran && retirement.callback.is_none() {
            retirement.callback = Some(Box::new(retire));
        }
        drop(retirement);
        self.queue.run_retirement_if_complete();
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
            reaper.initiate_shutdown(None);
        }
    }
}

impl RecorderReaperHandle {
    pub(crate) fn shutdown_control(&self) -> RecorderShutdownControl {
        RecorderShutdownControl {
            queue: Arc::clone(&self.queue),
            cleanup: self.cleanup.clone(),
        }
    }

    fn submit(
        &self,
        worker: RecorderWorker,
        worker_generation: u64,
        controller: Weak<RecorderController>,
        completion: Option<ReapCompletion>,
        completion_at_unix_ms: u64,
    ) {
        let supervisor = worker.into_supervisor();
        let deadline = Instant::now() + supervisor.shutdown_timeout();
        let task = ReapTask {
            supervisor: Some(supervisor),
            worker_generation,
            controller,
            completion,
            deadline,
            completion_at_unix_ms,
            cancelled: false,
        };
        match self.queue.submit(task) {
            Ok(()) => {}
            Err(ReaperSubmitError::Closed(task) | ReaperSubmitError::Failed(task)) => {
                self.cleanup
                    .submit_task(*task, Some(Arc::clone(&self.queue)));
            }
        }
    }

    fn try_submit(
        &self,
        worker: RecorderWorker,
        worker_generation: u64,
        controller: Weak<RecorderController>,
        completion_at_unix_ms: u64,
    ) -> Result<(), RecorderWorker> {
        let mut state = self
            .queue
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        if !state.accepting || state.failed || state.pending >= self.queue.capacity {
            return Err(worker);
        }
        let supervisor = worker.into_supervisor();
        let task = ReapTask {
            deadline: Instant::now() + supervisor.shutdown_timeout(),
            supervisor: Some(supervisor),
            worker_generation,
            controller,
            completion: None,
            completion_at_unix_ms,
            cancelled: false,
        };
        state.pending += 1;
        if let Err(error) = self.queue.sender.send(ReaperCommand::Reap(task)) {
            state.accepting = false;
            state.failed = true;
            state.shutdown_deadline = Some(Instant::now());
            drop(state);
            self.queue.available.notify_all();
            let ReaperCommand::Reap(mut task) = error.0 else {
                unreachable!("reserved submission only sends reap commands");
            };
            task.deadline = Instant::now();
            self.cleanup
                .submit_task(task, Some(Arc::clone(&self.queue)));
        }
        Ok(())
    }
}

impl RecorderShutdownControl {
    pub(crate) fn initiate_shutdown(&self, deadline: Instant) -> RtmpRecorderShutdown {
        self.queue.shutdown(Some(deadline));
        self.cleanup.wake();
        RtmpRecorderShutdown {
            queue: Arc::clone(&self.queue),
            retirement: Arc::clone(&self.queue.retirement),
        }
    }
}

impl RecorderCleanupHandle {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("rtmp-recorder-cleanup".to_owned())
            .spawn(move || run_cleanup(&receiver))
            .expect("recorder cleanup lifecycle must start");
        drop(thread);
        Self { sender }
    }

    fn submit_task(&self, task: ReapTask, queue: Option<Arc<ReaperQueue>>) {
        if let Err(error) = self.sender.send(CleanupCommand::Task { task, queue }) {
            let CleanupCommand::Task { task, queue } = error.0 else {
                unreachable!("task submission only sends cleanup tasks");
            };
            abandon_cleanup_task(task, queue);
        }
    }

    fn submit_reaper(&self, thread: JoinHandle<()>, queue: &Arc<ReaperQueue>) {
        if let Err(error) = self.sender.send(CleanupCommand::Reaper {
            thread,
            queue: Arc::clone(queue),
        }) {
            let CleanupCommand::Reaper { thread, .. } = error.0 else {
                unreachable!("reaper submission only sends reaper handles");
            };
            drop(thread);
        }
    }

    fn wake(&self) {
        let _ = self.sender.send(CleanupCommand::Wake);
    }
}

fn run_cleanup(receiver: &Receiver<CleanupCommand>) {
    let mut tasks = Vec::new();
    let mut reapers = Vec::new();
    let mut disconnected = false;
    loop {
        match receiver.recv_timeout(REAPER_POLL_INTERVAL) {
            Ok(command) => retain_cleanup_command(command, &mut tasks, &mut reapers),
            Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        while let Ok(command) = receiver.try_recv() {
            retain_cleanup_command(command, &mut tasks, &mut reapers);
        }

        let mut index = 0;
        while index < tasks.len() {
            let finished = tasks[index]
                .0
                .supervisor
                .as_ref()
                .is_some_and(RecorderWorkerSupervisor::is_finished);
            if !finished {
                let process_deadline = tasks[index]
                    .1
                    .as_ref()
                    .and_then(|queue| queue.shutdown_deadline());
                let deadline = earliest_deadline(Some(tasks[index].0.deadline), process_deadline)
                    .expect("a cleanup task always has a deadline");
                if Instant::now() >= deadline {
                    tasks[index].0.cancel();
                }
                index += 1;
                continue;
            }
            let (task, queue) = tasks.swap_remove(index);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                finish_cleanup_task(task, queue);
            }));
        }

        let mut index = 0;
        while index < reapers.len() {
            if !reapers[index].0.is_finished() {
                index += 1;
                continue;
            }
            let (thread, queue) = reapers.swap_remove(index);
            let _ = thread.join();
            queue.finish_reaper();
        }

        if disconnected && tasks.is_empty() && reapers.is_empty() {
            return;
        }
    }
}

fn retain_cleanup_command(
    command: CleanupCommand,
    tasks: &mut Vec<(ReapTask, Option<Arc<ReaperQueue>>)>,
    reapers: &mut Vec<(JoinHandle<()>, Arc<ReaperQueue>)>,
) {
    match command {
        CleanupCommand::Task { task, queue } => tasks.push((task, queue)),
        CleanupCommand::Reaper { thread, queue } => reapers.push((thread, queue)),
        CleanupCommand::Wake => {}
    }
}

fn finish_cleanup_task(task: ReapTask, queue: Option<Arc<ReaperQueue>>) {
    let _completion = QueueCompletion(queue);
    task.finish();
}

fn abandon_cleanup_task(mut task: ReapTask, queue: Option<Arc<ReaperQueue>>) {
    let _completion = QueueCompletion(queue);
    task.cancel();
    task.abandon();
}

struct QueueCompletion(Option<Arc<ReaperQueue>>);

impl Drop for QueueCompletion {
    fn drop(&mut self) {
        if let Some(queue) = self.0.take() {
            queue.complete();
        }
    }
}

impl ReaperQueue {
    fn admission(&self) -> Option<MutexGuard<'_, ReaperQueueState>> {
        let state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.accepting.then_some(state)
    }

    fn submit(&self, mut task: ReapTask) -> Result<(), ReaperSubmitError> {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state = self
            .available
            .wait_while(state, |state| {
                state.accepting && state.pending >= self.capacity
            })
            .expect("recorder reaper queue mutex poisoned");
        if state.failed {
            task.deadline = Instant::now();
            state.pending += 1;
            return Err(ReaperSubmitError::Failed(Box::new(task)));
        }
        if !state.accepting {
            task.deadline = earliest_deadline(Some(task.deadline), state.shutdown_deadline)
                .expect("a reaper task always has a deadline");
            state.pending += 1;
            return Err(ReaperSubmitError::Closed(Box::new(task)));
        }
        state.pending += 1;
        match self.sender.send(ReaperCommand::Reap(task)) {
            Ok(()) => Ok(()),
            Err(error) => {
                state.pending -= 1;
                drop(state);
                self.available.notify_all();
                let ReaperCommand::Reap(mut task) = error.0 else {
                    unreachable!("submit only sends reap commands");
                };
                task.deadline = Instant::now();
                state = self
                    .state
                    .lock()
                    .expect("recorder reaper queue mutex poisoned");
                state.accepting = false;
                state.failed = true;
                drop(state);
                self.available.notify_all();
                Err(ReaperSubmitError::Failed(Box::new(task)))
            }
        }
    }

    fn shutdown(&self, deadline: Option<Instant>) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.accepting = false;
        state.shutdown_deadline = earliest_deadline(state.shutdown_deadline, deadline);
        let effective_deadline = state.shutdown_deadline;
        let _ = self.sender.send(ReaperCommand::Shutdown {
            deadline: effective_deadline,
        });
        drop(state);
        self.available.notify_all();
        self.run_retirement_if_complete();
    }

    fn shutdown_deadline(&self) -> Option<Instant> {
        self.state
            .lock()
            .expect("recorder reaper queue mutex poisoned")
            .shutdown_deadline
    }

    fn complete(&self) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.pending = state
            .pending
            .checked_sub(1)
            .expect("recorder reaper completed an untracked task");
        drop(state);
        self.available.notify_one();
        self.run_retirement_if_complete();
    }

    fn fail(&self) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.accepting = false;
        state.failed = true;
        state.shutdown_deadline = Some(Instant::now());
        drop(state);
        self.available.notify_all();
    }

    fn finish_reaper(&self) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.reaper_finished = true;
        drop(state);
        self.available.notify_all();
        self.run_retirement_if_complete();
    }

    fn run_retirement_if_complete(&self) {
        if !self.shutdown_complete() {
            return;
        }
        let retirement = {
            let mut state = self
                .retirement
                .lock()
                .expect("recorder retirement mutex poisoned");
            if state.ran {
                None
            } else if let Some(callback) = state.callback.take() {
                state.ran = true;
                Some(callback)
            } else {
                None
            }
        };
        if let Some(retirement) = retirement {
            retirement();
        }
    }

    fn shutdown_complete(&self) -> bool {
        let state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        !state.accepting && state.pending == 0 && state.reaper_finished
    }

    fn wait_until_shutdown_complete(&self, deadline: Instant) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        loop {
            if !state.accepting && state.pending == 0 && state.reaper_finished {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .available
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("recorder reaper queue mutex poisoned")
                .0;
        }
    }

    #[cfg(test)]
    fn panic_reaper(&self) {
        let _ = self.sender.send(ReaperCommand::Panic);
    }

    #[cfg(test)]
    fn status(&self) -> (bool, usize) {
        let state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        (state.accepting, state.pending)
    }
}

impl ReapTask {
    fn cancel(&mut self) {
        if self.cancelled {
            return;
        }
        self.supervisor
            .as_ref()
            .expect("unfinished reaper task owns a supervisor")
            .cancel();
        self.cancelled = true;
    }

    fn finish(mut self) {
        let status = self
            .supervisor
            .take()
            .expect("unfinished reaper task owns a supervisor")
            .join();
        if let Some(controller) = self.controller.upgrade() {
            controller.finish_reap(
                self.worker_generation,
                status.clone(),
                self.completion_at_unix_ms,
            );
        }
        if let Some(completion) = self.completion
            && let Some(registry) = completion.registry.upgrade()
        {
            registry.complete_worker_stop(completion.context, &status);
        }
    }

    fn abandon(mut self) {
        let status = self
            .supervisor
            .take()
            .expect("unfinished reaper task owns a supervisor")
            .detach();
        if let Some(controller) = self.controller.upgrade() {
            controller.finish_reap(
                self.worker_generation,
                status.clone(),
                self.completion_at_unix_ms,
            );
        }
        if let Some(completion) = self.completion
            && let Some(registry) = completion.registry.upgrade()
        {
            registry.complete_worker_stop(completion.context, &status);
        }
    }
}

fn supervise_reaper(
    receiver: &Receiver<ReaperCommand>,
    queue: &Arc<ReaperQueue>,
    cleanup: &RecorderCleanupHandle,
) {
    let mut tasks = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_reaper(receiver, queue, &mut tasks);
    }));
    if result.is_ok() {
        return;
    }
    queue.fail();
    for task in tasks.drain(..) {
        let mut task = task;
        task.deadline = Instant::now();
        cleanup.submit_task(task, Some(Arc::clone(queue)));
    }
    while let Ok(command) = receiver.try_recv() {
        if let ReaperCommand::Reap(task) = command {
            let mut task = task;
            task.deadline = Instant::now();
            cleanup.submit_task(task, Some(Arc::clone(queue)));
        }
    }
}

fn run_reaper(
    receiver: &Receiver<ReaperCommand>,
    queue: &Arc<ReaperQueue>,
    tasks: &mut Vec<ReapTask>,
) {
    let mut shutting_down = false;
    let mut shutdown_deadline: Option<Instant> = None;
    loop {
        match receiver.recv_timeout(REAPER_POLL_INTERVAL) {
            Ok(ReaperCommand::Reap(task)) => tasks.push(task),
            Ok(ReaperCommand::Shutdown { deadline }) => {
                shutting_down = true;
                shutdown_deadline = earliest_deadline(shutdown_deadline, deadline);
            }
            #[cfg(test)]
            Ok(ReaperCommand::Panic) => panic!("injected recorder reaper panic"),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                shutting_down = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        loop {
            match receiver.try_recv() {
                Ok(ReaperCommand::Reap(task)) => tasks.push(task),
                Ok(ReaperCommand::Shutdown { deadline }) => {
                    shutting_down = true;
                    shutdown_deadline = earliest_deadline(shutdown_deadline, deadline);
                    break;
                }
                #[cfg(test)]
                Ok(ReaperCommand::Panic) => panic!("injected recorder reaper panic"),
                Err(TryRecvError::Disconnected) => {
                    shutting_down = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        let now = Instant::now();
        for task in &mut *tasks {
            if now >= task.deadline || shutdown_deadline.is_some_and(|deadline| now >= deadline) {
                task.cancel();
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
            finish_cleanup_task(tasks.swap_remove(index), Some(Arc::clone(queue)));
        }
        if shutting_down && tasks.is_empty() {
            return;
        }
    }
}

fn earliest_deadline(current: Option<Instant>, next: Option<Instant>) -> Option<Instant> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current.min(next)),
        (Some(current), None) => Some(current),
        (None, next) => next,
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

fn recorder_start_error_code(error: &crate::RecorderWorkerStartError) -> RecorderErrorCode {
    match error {
        crate::RecorderWorkerStartError::UnsupportedVideoCodec(_) => {
            RecorderErrorCode::UnsupportedCodec
        }
        crate::RecorderWorkerStartError::ThreadSpawn(_) => RecorderErrorCode::BackendUnavailable,
        crate::RecorderWorkerStartError::Path(_)
        | crate::RecorderWorkerStartError::Capacity(_)
        | crate::RecorderWorkerStartError::InvalidQueueLimits
        | crate::RecorderWorkerStartError::InvalidRotationInterval
        | crate::RecorderWorkerStartError::InvalidShutdownTimeout
        | crate::RecorderWorkerStartError::InvalidRecordMask
        | crate::RecorderWorkerStartError::InvalidRecordingLimit => RecorderErrorCode::OpenFailed,
    }
}

fn accumulate_worker_status(
    completed: Option<&RecorderWorkerStatus>,
    mut current: RecorderWorkerStatus,
) -> RecorderWorkerStatus {
    let Some(completed) = completed else {
        return current;
    };
    current.events_enqueued = completed
        .events_enqueued
        .saturating_add(current.events_enqueued);
    current.events_processed = completed
        .events_processed
        .saturating_add(current.events_processed);
    current.events_dropped = completed
        .events_dropped
        .saturating_add(current.events_dropped);
    current.bytes_written = completed
        .bytes_written
        .saturating_add(current.bytes_written);
    current.segments_started = completed
        .segments_started
        .saturating_add(current.segments_started);
    current.segments_completed = completed
        .segments_completed
        .saturating_add(current.segments_completed);
    current.discontinuities = completed
        .discontinuities
        .saturating_add(current.discontinuities);
    if current.last_completed_relative_name.is_none() {
        current
            .last_completed_relative_name
            .clone_from(&completed.last_completed_relative_name);
    }
    if current.recoverable_partial_name.is_none() {
        current
            .recoverable_partial_name
            .clone_from(&completed.recoverable_partial_name);
    }
    if current.published_but_not_durable_relative_name.is_none() {
        current
            .published_but_not_durable_relative_name
            .clone_from(&completed.published_but_not_durable_relative_name);
    }
    current
}

fn media_events_fit(events: &[MediaEvent], config: RecorderWorkerConfig) -> bool {
    events.len() <= config.max_queue_messages
        && events
            .iter()
            .try_fold(0_usize, |bytes, event| {
                bytes.checked_add(event.payload_len())
            })
            .is_some_and(|bytes| bytes <= config.max_queue_bytes)
}

fn enqueue_bootstrap_replay(
    bootstrap: &mut RecorderBootstrap,
    worker: &RecorderWorker,
    replay: &mut BootstrapReplay,
    at_unix_ms: u64,
) {
    for event in replay.events.drain(..) {
        let kind = event.kind();
        let result = worker.try_enqueue_at(event, Instant::now(), at_unix_ms);
        if result != RecorderEnqueueResult::Queued {
            bootstrap.mark_recovery_gap(kind, result);
            replay.reserved_event_fits = false;
            break;
        }
    }
}

fn record_recovery_drop(state: &mut ControllerState, event: &MediaEvent) {
    let kind = event.kind();
    state.bootstrap.update(event);
    state
        .bootstrap
        .mark_recovery_gap(kind, RecorderEnqueueResult::Inactive);
    if state.restart_context.is_some() {
        state.recovery_events_dropped = state.recovery_events_dropped.saturating_add(1);
    }
}

fn count_inactive_recovery_drop(state: &mut ControllerState, result: RecorderEnqueueResult) {
    if result == RecorderEnqueueResult::Inactive {
        state.recovery_events_dropped = state.recovery_events_dropped.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
    };

    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{
        RecorderEnqueueResult, RecorderFailure, RecorderWorkerPhase, RecordingPathPolicy,
        RecordingStore, RecordingStoreLimits, RtmpCapabilities,
    };

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);

    #[test]
    fn recovery_bootstrap_discards_an_oversized_snapshot_without_blocking_start() {
        let mut bootstrap = RecorderBootstrap::default();
        bootstrap.update(
            &MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x00\x12"[..])).expect("AAC header"),
        );
        bootstrap.update(
            &MediaEvent::video(0, Arc::<[u8]>::from(&b"\x17\x00\x00\x00\x00\x01"[..]))
                .expect("AVC header"),
        );

        let config = RecorderWorkerConfig {
            max_queue_messages: 2,
            max_queue_bytes: 1024,
            rotation_interval: None,
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        };
        let reserved =
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"current"[..])).expect("current audio");
        let replay = bootstrap
            .replay_events(config, Some(&reserved))
            .expect("bootstrap fits without the reserved event");

        assert_eq!(replay.events.len(), 2);
        assert!(!replay.reserved_event_fits);
        assert_eq!(bootstrap.events().count(), 2);

        assert!(
            bootstrap
                .replay_events(
                    RecorderWorkerConfig {
                        max_queue_messages: 1,
                        ..config
                    },
                    Some(&reserved)
                )
                .is_none()
        );
        assert_eq!(bootstrap.events().count(), 2);
    }

    #[test]
    fn recovery_gap_rejects_a_stale_keyframe_but_keeps_a_new_one() {
        let mut bootstrap = RecorderBootstrap::default();
        let stale =
            MediaEvent::video(0, Arc::<[u8]>::from(&b"\x17\x01stale"[..])).expect("stale keyframe");
        bootstrap.update(&stale);
        let audio = MediaEvent::audio(1, Arc::<[u8]>::from(&b"audio"[..])).expect("audio");
        bootstrap.mark_recovery_gap(audio.kind(), RecorderEnqueueResult::Inactive);
        assert!(bootstrap.keyframe.is_none());

        let current = MediaEvent::video(2, Arc::<[u8]>::from(&b"\x17\x01current"[..]))
            .expect("current keyframe");
        bootstrap.update(&current);
        bootstrap.mark_recovery_gap(current.kind(), RecorderEnqueueResult::Inactive);
        assert_eq!(bootstrap.keyframe, Some(current));
    }

    #[test]
    fn publication_claim_wins_over_process_deadline_cancellation() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        let gate = fixture.prepare_publication();
        let control = fixture.reaper.shutdown_control();
        fixture.controller.deactivate(1_100);
        assert!(gate.wait_before_claim(TEST_TIMEOUT));
        gate.allow_claim();
        assert!(gate.wait_after_claim(TEST_TIMEOUT));

        let budget = Duration::from_millis(250);
        let started = Instant::now();
        fixture.shutdown_owner(None);
        let shutdown = control.initiate_shutdown(started + budget);
        assert!(started.elapsed() < Duration::from_millis(50));
        wait_until(TEST_TIMEOUT, || started.elapsed() >= budget);
        assert!(!matches!(
            fixture
                .controller
                .status()
                .status
                .map(|status| status.phase),
            Some(RecorderWorkerPhase::Failed(
                RecorderFailure::ShutdownTimedOut
            ))
        ));
        gate.allow_publication();
        assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));

        let status = fixture
            .controller
            .status()
            .status
            .expect("completed recorder status");
        assert_eq!(status.phase, RecorderWorkerPhase::Stopped);
        assert_eq!(status.segments_completed, 1);
        assert!(fixture.root.path().join("camera.flv").is_file());
        assert!(partial_files(fixture.root.path()).is_empty());
        assert_eq!(fixture.reaper.queue.status(), (false, 0));
    }

    #[test]
    fn shutdown_control_monotonically_tightens_after_owner_drop() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (owner, reaper) = registry.create_recorder_reaper(1);
        let control = reaper.shutdown_control();
        drop(owner);
        let now = Instant::now();
        let first = now + Duration::from_millis(400);
        let later = now + Duration::from_millis(800);
        let earlier = now + Duration::from_millis(200);

        drop(control.initiate_shutdown(first));
        assert_eq!(reaper.queue.shutdown_deadline(), Some(first));
        drop(control.initiate_shutdown(later));
        assert_eq!(reaper.queue.shutdown_deadline(), Some(first));
        drop(control.initiate_shutdown(earlier));
        assert_eq!(reaper.queue.shutdown_deadline(), Some(earlier));
        assert!(!reaper.queue.status().0);
    }

    #[test]
    fn shutdown_control_tightens_long_owner_drop_cleanup_deadline() {
        let mut fixture = RecorderFixture::new(Duration::from_secs(5));
        let gate = fixture.prepare_publication();
        let control = fixture.reaper.shutdown_control();
        fixture.shutdown_owner(None);
        fixture.controller.deactivate(1_100);
        assert!(gate.wait_before_claim(TEST_TIMEOUT));
        let budget = Duration::from_millis(60);
        let started = Instant::now();
        let shutdown = control.initiate_shutdown(started + budget);

        assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));
        assert!(started.elapsed() >= budget.saturating_sub(Duration::from_millis(10)));
        assert!(started.elapsed() < Duration::from_millis(250));
        let status = fixture
            .controller
            .status()
            .status
            .expect("cancelled recorder status");
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
        );
        assert!(status.recoverable_partial_name.is_some());
    }

    #[test]
    fn recorder_retirement_runs_once_across_shutdown_handle_clones() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (owner, _reaper) = registry.create_recorder_reaper(1);
        let shutdown = owner.shutdown_handle();
        let clone = shutdown.clone();
        let retired = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&retired);
        shutdown.set_retirement(move || {
            observed.fetch_add(1, Ordering::AcqRel);
        });

        let deadline = Instant::now() + TEST_TIMEOUT;
        drop(owner.initiate_shutdown(deadline));
        assert!(shutdown.wait_until(deadline));
        assert!(clone.wait_until(deadline));
        assert_eq!(retired.load(Ordering::Acquire), 1);
    }

    #[test]
    fn cancellation_claim_wins_at_the_existing_task_deadline() {
        let shutdown_timeout = Duration::from_millis(80);
        let mut fixture = RecorderFixture::new(shutdown_timeout);
        let gate = fixture.prepare_publication();
        let started = Instant::now();
        fixture.controller.deactivate(1_100);
        assert!(gate.wait_before_claim(TEST_TIMEOUT));

        let owner_drop_started = Instant::now();
        fixture.shutdown_owner(None);
        assert!(owner_drop_started.elapsed() < Duration::from_millis(50));

        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        assert!(started.elapsed() >= shutdown_timeout);
        assert!(started.elapsed() < TEST_TIMEOUT);
        let status = fixture
            .controller
            .status()
            .status
            .expect("cancelled recorder status");
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
        );
        let partial = status
            .recoverable_partial_name
            .expect("deadline cancellation preserves the partial");
        assert!(fixture.root.path().join(partial).is_file());
        assert!(fixture.root.path().join("camera.flv").is_file());
        assert_eq!(fixture.reaper.queue.status(), (false, 0));
    }

    #[test]
    fn process_deadline_caps_a_longer_task_deadline() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        let gate = fixture.prepare_publication();
        fixture.controller.deactivate(1_100);
        assert!(gate.wait_before_claim(TEST_TIMEOUT));

        let budget = Duration::from_millis(60);
        let started = Instant::now();
        fixture.shutdown_owner(Some(started + budget));

        assert!(started.elapsed() < Duration::from_millis(50));
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        assert!(started.elapsed() >= budget.saturating_sub(Duration::from_millis(10)));
        assert!(started.elapsed() < Duration::from_millis(250));
        let status = fixture
            .controller
            .status()
            .status
            .expect("cancelled recorder status");
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
        );
        assert!(status.recoverable_partial_name.is_some());
        assert!(fixture.root.path().join("camera.flv").is_file());
    }

    #[test]
    fn reaper_rejects_and_cleans_up_submissions_after_shutdown() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        fixture.prepare_publication();
        fixture.shutdown_owner(Some(Instant::now()));
        assert_eq!(fixture.reaper.queue.status(), (false, 0));

        let started = Instant::now();
        fixture.controller.deactivate(1_100);

        assert!(started.elapsed() < Duration::from_millis(100));
        wait_until(TEST_TIMEOUT, || {
            fixture.controller.status().status.is_some_and(|status| {
                matches!(
                    status.phase,
                    RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
                )
            })
        });
        let status = fixture
            .controller
            .status()
            .status
            .expect("rejected recorder status");
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
        );
        let partial = status
            .recoverable_partial_name
            .expect("rejected submission preserves the partial");
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        assert!(fixture.root.path().join(partial).is_file());
        assert!(fixture.root.path().join("camera.flv").is_file());
    }

    #[test]
    fn shutdown_closes_intake_and_wakes_a_capacity_blocked_submitter() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        let gate = fixture.prepare_publication();
        assert!(fixture.controller.stop(recorder_context(1_100)));
        assert!(gate.wait_before_claim(TEST_TIMEOUT));
        fixture
            .controller
            .start(recorder_context(1_200))
            .expect("start replacement recorder worker");
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"replacement"[..]))
                    .expect("replacement audio event"),
                1_250,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture
                .controller
                .status()
                .status
                .is_some_and(|status| status.bytes_written > 13)
        });

        let controller = Arc::clone(&fixture.controller);
        let (submitted_tx, submitted_rx) = mpsc::channel();
        let submitter = thread::spawn(move || {
            assert!(controller.stop(recorder_context(1_300)));
            submitted_tx.send(()).expect("report blocked submission");
        });
        assert!(matches!(
            submitted_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        fixture.shutdown_owner(Some(Instant::now() + Duration::from_millis(60)));
        submitted_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("closed intake wakes submitter");
        submitter.join().expect("reaper submitter");
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        assert_eq!(fixture.reaper.queue.status(), (false, 0));
    }

    #[test]
    fn stale_reap_completion_does_not_overwrite_replacement_worker_status() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(80));
        let gate = fixture.prepare_publication();
        assert!(fixture.controller.stop(recorder_context(1_100)));
        assert!(gate.wait_before_claim(TEST_TIMEOUT));

        fixture
            .controller
            .start(recorder_context(1_200))
            .expect("start replacement recorder worker");
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"replacement"[..]))
                    .expect("replacement audio event"),
                1_250,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);

        let replacement = fixture.controller.status();
        let replacement_status = replacement.status.expect("replacement worker status");
        assert!(!replacement.stopping);
        assert_eq!(replacement_status.phase, RecorderWorkerPhase::Recording);
        assert!(replacement_status.bytes_written > 13);

        fixture.controller.deactivate(1_300);
        fixture.shutdown_owner(None);
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        assert!(fixture.root.path().join("camera.flv").is_file());
    }

    #[test]
    fn capacity_one_restart_waits_for_the_previous_worker_lease() {
        let mut fixture = RecorderFixture::with_max_active(Duration::from_millis(40), 1);
        let gate = fixture.prepare_publication();
        assert!(fixture.controller.stop(recorder_context(1_100)));
        assert!(gate.wait_before_claim(TEST_TIMEOUT));

        let (started_tx, started_rx) = mpsc::channel();
        let mut restarts = Vec::new();
        for _ in 0..2 {
            let controller = Arc::clone(&fixture.controller);
            let started_tx = started_tx.clone();
            restarts.push(thread::spawn(move || {
                started_tx
                    .send(controller.start(recorder_context(1_200)))
                    .expect("report replacement start");
            }));
        }
        assert!(matches!(
            started_rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        for _ in 0..2 {
            started_rx
                .recv_timeout(TEST_TIMEOUT)
                .expect("replacement starts after prior lease is reaped")
                .expect("start replacement recorder");
        }
        for restart in restarts {
            restart.join().expect("replacement start thread");
        }

        fixture.controller.deactivate(1_300);
        fixture.shutdown_owner(None);
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    }

    #[test]
    fn shutdown_wakes_a_capacity_one_restart_waiter() {
        let mut fixture = RecorderFixture::with_max_active(Duration::from_millis(500), 1);
        let gate = fixture.prepare_publication();
        assert!(fixture.controller.stop(recorder_context(1_100)));
        assert!(gate.wait_before_claim(TEST_TIMEOUT));

        let controller = Arc::clone(&fixture.controller);
        let (started_tx, started_rx) = mpsc::channel();
        let restart = thread::spawn(move || {
            started_tx
                .send(controller.start(recorder_context(1_200)))
                .expect("report replacement start");
        });
        assert!(matches!(
            started_rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let shutdown = fixture.initiate_shutdown_owner(Instant::now());
        assert_eq!(
            started_rx
                .recv_timeout(TEST_TIMEOUT)
                .expect("shutdown wakes replacement start"),
            Err(RecorderErrorCode::BackendUnavailable)
        );
        restart.join().expect("replacement start thread");
        assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));
    }

    #[test]
    fn continuous_recovery_resumes_the_existing_file_inside_the_interval() {
        assert_continuous_recovery(50_000, "camera-1.flv");
    }

    #[test]
    fn continuous_recovery_starts_the_current_file_after_the_interval() {
        assert_continuous_recovery(62_000, "camera-62.flv");
    }

    #[test]
    fn continuous_recovery_backs_off_while_the_reaper_is_saturated() {
        let mut fixture = RecorderFixture::with_max_active(Duration::from_millis(500), 1);
        fixture.controller.fail_before_process();
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"failure"[..])).expect("failure audio"),
                1_100,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture.controller.status().status.is_some_and(|status| {
                status.phase == RecorderWorkerPhase::Failed(RecorderFailure::Open)
            })
        });
        fixture
            .reaper
            .queue
            .state
            .lock()
            .expect("reaper state")
            .pending = 1;

        let worker_generation = fixture.controller.lock().worker_generation;
        let started = Instant::now();
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(1, Arc::<[u8]>::from(&b"saturated"[..]))
                    .expect("saturated audio"),
                1_200,
            ),
            RecorderEnqueueResult::Inactive
        );
        assert!(started.elapsed() < Duration::from_millis(20));
        assert!(fixture.controller.lock().worker.is_some());
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(2, Arc::<[u8]>::from(&b"backoff"[..])).expect("backoff audio"),
                1_201,
            ),
            RecorderEnqueueResult::Inactive
        );
        assert_eq!(
            fixture.controller.lock().worker_generation,
            worker_generation
        );

        fixture
            .reaper
            .queue
            .state
            .lock()
            .expect("reaper state")
            .pending = 0;
        fixture.reaper.queue.available.notify_all();
        thread::sleep(CONTINUOUS_RESTART_DELAY);
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(3, Arc::<[u8]>::from(&b"retry"[..])).expect("retry audio"),
                1_300,
            ),
            RecorderEnqueueResult::Inactive
        );
        wait_until(TEST_TIMEOUT, || {
            let status = fixture.controller.status();
            !status.stopping && status.recovering
        });
        thread::sleep(CONTINUOUS_RESTART_DELAY);
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(4, Arc::<[u8]>::from(&b"continued"[..]))
                    .expect("continued audio"),
                1_400,
            ),
            RecorderEnqueueResult::Queued
        );
        fixture.controller.deactivate(1_500);
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        fixture.shutdown_owner(None);
    }

    #[test]
    fn reaper_panic_closes_intake_and_transfers_pending_cleanup() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        let gate = fixture.prepare_publication();
        assert!(fixture.controller.stop(recorder_context(1_100)));
        assert!(gate.wait_before_claim(TEST_TIMEOUT));
        fixture
            .controller
            .start(recorder_context(1_200))
            .expect("start replacement recorder worker");

        let controller = Arc::clone(&fixture.controller);
        let (submitted_tx, submitted_rx) = mpsc::channel();
        let submitter = thread::spawn(move || {
            assert!(controller.stop(recorder_context(1_300)));
            submitted_tx.send(()).expect("report failed submission");
        });
        assert!(matches!(
            submitted_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        fixture.reaper.queue.panic_reaper();
        submitted_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("reaper panic wakes blocked submitter");
        submitter.join().expect("reaper submitter");
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        assert_eq!(fixture.reaper.queue.status(), (false, 0));
        fixture.shutdown_owner(None);
    }

    #[test]
    fn segment_finish_panic_reports_the_preserved_partial() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
                1_050,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture
                .controller
                .status()
                .status
                .is_some_and(|status| status.bytes_written > 13)
        });
        fixture.controller.panic_on_finish();

        fixture.controller.deactivate(1_100);
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);

        let status = fixture
            .controller
            .status()
            .status
            .expect("panicked recorder status");
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::WorkerPanicked)
        );
        let partial = status
            .recoverable_partial_name
            .expect("finish panic preserves the current partial");
        assert!(fixture.root.path().join(partial).is_file());
        assert!(fixture.root.path().join("camera.flv").is_file());
        fixture.shutdown_owner(None);
    }

    #[test]
    fn post_finish_panic_does_not_report_the_removed_partial() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
                1_050,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture
                .controller
                .status()
                .status
                .is_some_and(|status| status.bytes_written > 13)
        });
        fixture.controller.panic_after_finish();

        fixture.controller.deactivate(1_100);
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);

        let status = fixture
            .controller
            .status()
            .status
            .expect("panicked recorder status");
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::WorkerPanicked)
        );
        assert_eq!(status.recoverable_partial_name, None);
        assert!(fixture.root.path().join("camera.flv").is_file());
        assert!(partial_files(fixture.root.path()).is_empty());
        fixture.shutdown_owner(None);
    }

    #[test]
    fn rejected_cleanup_progresses_while_the_reaper_is_still_shutting_down() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        let gate = fixture.prepare_publication();
        assert!(fixture.controller.stop(recorder_context(1_100)));
        assert!(gate.wait_before_claim(TEST_TIMEOUT));
        gate.allow_claim();
        assert!(gate.wait_after_claim(TEST_TIMEOUT));

        fixture
            .controller
            .start(recorder_context(1_200))
            .expect("start replacement recorder worker");
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"replacement"[..]))
                    .expect("replacement audio event"),
                1_250,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture
                .controller
                .status()
                .status
                .is_some_and(|status| status.bytes_written > 13)
        });

        fixture.shutdown_owner(Some(Instant::now()));
        assert_eq!(fixture.reaper.queue.status(), (false, 1));
        assert!(fixture.controller.stop(recorder_context(1_300)));
        wait_until(TEST_TIMEOUT, || {
            let status = fixture.controller.status();
            !status.stopping
                && status.status.is_some_and(|status| {
                    matches!(
                        status.phase,
                        RecorderWorkerPhase::Stopped
                            | RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
                    )
                })
        });
        assert_eq!(fixture.reaper.queue.status(), (false, 1));

        gate.allow_publication();
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    }

    #[test]
    fn shutdown_completion_waits_for_cleanup_but_respects_its_absolute_deadline() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        let gate = fixture.prepare_publication();
        fixture.controller.deactivate(1_100);
        assert!(gate.wait_before_claim(TEST_TIMEOUT));
        gate.allow_claim();
        assert!(gate.wait_after_claim(TEST_TIMEOUT));
        let shutdown = fixture.initiate_shutdown_owner(Instant::now() + TEST_TIMEOUT);

        assert!(!shutdown.is_complete());
        let wait_started = Instant::now();
        assert!(!shutdown.wait_until(wait_started + Duration::from_millis(40)));
        assert!(wait_started.elapsed() >= Duration::from_millis(30));

        gate.allow_publication();
        assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));
        assert!(shutdown.is_complete());
    }

    #[test]
    fn recorder_start_after_shutdown_snapshot_is_terminally_rejected() {
        let mut fixture = RecorderFixture::new(Duration::from_millis(500));
        let shutdown = fixture.initiate_shutdown_owner(Instant::now() + TEST_TIMEOUT);
        assert!(fixture.controller.stop(recorder_context(1_100)));
        assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));

        assert_eq!(
            fixture.controller.start(recorder_context(1_200)),
            Err(RecorderErrorCode::BackendUnavailable)
        );
    }

    struct RecorderFixture {
        root: TempDir,
        owner: Option<Arc<RecorderReaperOwner>>,
        reaper: RecorderReaperHandle,
        controller: Arc<RecorderController>,
    }

    impl RecorderFixture {
        fn new(shutdown_timeout: Duration) -> Self {
            Self::with_max_active(shutdown_timeout, 2)
        }

        fn with_max_active(shutdown_timeout: Duration, max_active_recorders: usize) -> Self {
            Self::with_recording_policy(shutdown_timeout, max_active_recorders, None, false)
        }

        fn with_recording_policy(
            shutdown_timeout: Duration,
            max_active_recorders: usize,
            rotation_interval: Option<Duration>,
            append_unix_seconds: bool,
        ) -> Self {
            let root = tempdir().expect("recording root");
            let store = RecordingStore::open(
                root.path(),
                RecordingStoreLimits {
                    max_bytes: Some(1024 * 1024),
                    max_files: Some(8),
                    max_active_recorders,
                },
            )
            .expect("recording store");
            let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
                live_ingest: true,
                manual_recording: true,
            }));
            let (owner, reaper) = registry.create_recorder_reaper(1);
            let controller = Arc::new(RecorderController::new(
                RtmpRecorderPolicy::new(
                    "archive",
                    RtmpRecorderStart::Continuous,
                    store,
                    RecordingPathPolicy::new(".flv", append_unix_seconds)
                        .expect("recording path policy"),
                    RecorderWorkerConfig {
                        max_queue_messages: 4,
                        max_queue_bytes: 1024,
                        rotation_interval,
                        shutdown_timeout,
                        video_codec: None,
                        ..RecorderWorkerConfig::default()
                    },
                ),
                Arc::<[u8]>::from(&b"camera"[..]),
                reaper.clone(),
                1_000,
            ));
            controller
                .start(recorder_context(1_000))
                .expect("start recorder worker");
            Self {
                root,
                owner: Some(owner),
                reaper,
                controller,
            }
        }

        fn prepare_publication(&self) -> Arc<crate::recording_store::RecordingPublicationGate> {
            assert_eq!(
                self.controller.try_enqueue(
                    MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
                    1_050,
                ),
                RecorderEnqueueResult::Queued
            );
            wait_until(TEST_TIMEOUT, || {
                self.controller
                    .status()
                    .status
                    .is_some_and(|status| status.segments_started == 1 && status.bytes_written > 13)
            });
            self.controller.install_publication_gate()
        }

        fn shutdown_owner(&mut self, deadline: Option<Instant>) {
            let owner = self.owner.take().expect("recorder reaper owner");
            if let Some(deadline) = deadline {
                let _ = owner.initiate_shutdown(deadline);
            }
            drop(owner);
        }

        fn initiate_shutdown_owner(&mut self, deadline: Instant) -> RtmpRecorderShutdown {
            let owner = self.owner.take().expect("recorder reaper owner");
            let shutdown = owner.initiate_shutdown(deadline);
            drop(owner);
            shutdown
        }
    }

    fn recorder_context(at_unix_ms: u64) -> RecorderCommandContext {
        RecorderCommandContext {
            stream_id: StreamId::new(),
            publisher_session_id: SessionId::new(),
            recorder_id: RecorderId::new(),
            operation_id: OperationId::new(),
            at_unix_ms,
        }
    }

    fn assert_continuous_recovery(recovery_at_unix_ms: u64, expected_file: &str) {
        let mut fixture = RecorderFixture::with_recording_policy(
            Duration::from_millis(500),
            1,
            Some(Duration::from_mins(1)),
            true,
        );
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"first"[..])).expect("initial audio"),
                1_050,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture
                .controller
                .status()
                .status
                .is_some_and(|status| status.bytes_written > 13)
        });
        assert!(fixture.controller.stop(recorder_context(1_100)));
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        let initial_file = fixture.root.path().join("camera-1.flv");
        let initial_size = fs::metadata(&initial_file)
            .expect("initial interval file")
            .len();

        fixture
            .controller
            .start(recorder_context(1_200))
            .expect("replacement recorder");
        fixture.controller.fail_before_process();
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(1, Arc::<[u8]>::from(&b"failure"[..])).expect("failure audio"),
                1_250,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture.controller.status().status.is_some_and(|status| {
                status.phase == RecorderWorkerPhase::Failed(RecorderFailure::Open)
            })
        });

        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(2, Arc::<[u8]>::from(&b"recover"[..])).expect("recovery trigger"),
                recovery_at_unix_ms,
            ),
            RecorderEnqueueResult::Inactive
        );
        wait_until(TEST_TIMEOUT, || {
            let status = fixture.controller.status();
            !status.stopping && status.recovering
        });
        let failed_generation = fixture.controller.lock().worker_generation;
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(3, Arc::<[u8]>::from(&b"backoff"[..])).expect("backoff audio"),
                recovery_at_unix_ms + 1,
            ),
            RecorderEnqueueResult::Inactive
        );
        assert_eq!(
            fixture.controller.lock().worker_generation,
            failed_generation,
            "recovery restarted before its bounded delay"
        );
        thread::sleep(CONTINUOUS_RESTART_DELAY);
        assert_eq!(
            fixture.controller.try_enqueue(
                MediaEvent::audio(4, Arc::<[u8]>::from(&b"continued"[..]))
                    .expect("continued audio"),
                recovery_at_unix_ms + 2,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            fixture.controller.status().status.is_some_and(|status| {
                status.phase == RecorderWorkerPhase::Recording && status.bytes_written > 13
            })
        });

        assert_recovery_output(
            &mut fixture,
            recovery_at_unix_ms,
            expected_file,
            &initial_file,
            initial_size,
        );
    }

    fn assert_recovery_output(
        fixture: &mut RecorderFixture,
        recovery_at_unix_ms: u64,
        expected_file: &str,
        initial_file: &Path,
        initial_size: u64,
    ) {
        fixture.controller.deactivate(recovery_at_unix_ms + 3);
        wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
        let recovered_status = fixture
            .controller
            .status()
            .status
            .expect("recovered recorder status");
        assert!(recovered_status.events_dropped >= 3);
        assert_eq!(recovered_status.segments_started, 2);
        assert!(recovered_status.bytes_written > initial_size);
        assert!(fixture.root.path().join(expected_file).is_file());
        if expected_file == "camera-1.flv" {
            assert!(
                fs::metadata(initial_file)
                    .expect("resumed interval file")
                    .len()
                    > initial_size
            );
        } else {
            assert_eq!(
                fs::metadata(initial_file)
                    .expect("prior interval file")
                    .len(),
                initial_size
            );
        }
        fixture.shutdown_owner(None);
    }

    fn partial_files(root: &Path) -> Vec<std::path::PathBuf> {
        fs::read_dir(root)
            .expect("recording root")
            .map(|entry| entry.expect("recording entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".partial"))
            })
            .collect()
    }

    fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + timeout;
        while !predicate() {
            assert!(Instant::now() < deadline, "condition timeout");
            thread::sleep(REAPER_POLL_INTERVAL);
        }
    }
}
