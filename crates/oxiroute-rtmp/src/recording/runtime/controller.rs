#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) struct RecorderController {
    policy: RtmpRecorderPolicy,
    stream_name: Arc<[u8]>,
    reaper: RecorderReaperHandle,
    state: Mutex<ControllerState>,
    state_changed: Condvar,
    last_observed_at_unix_ms: AtomicU64,
}

pub(super) struct ControllerState {
    active: bool,
    bootstrap: RecorderBootstrap,
    stopping: bool,
    pub(super) worker: Option<RecorderWorker>,
    pub(super) worker_generation: u64,
    last_status: Option<RecorderWorkerStatus>,
    completed_status: Option<RecorderWorkerStatus>,
    restart_context: Option<RecorderCommandContext>,
    restart_after: Instant,
    recovering: bool,
    recovery_events_dropped: u64,
}

#[derive(Default)]
pub(super) struct RecorderBootstrap {
    metadata: Option<MediaEvent>,
    aac: Option<MediaEvent>,
    video: Option<MediaEvent>,
    pub(super) keyframe: Option<MediaEvent>,
}

pub(super) struct BootstrapReplay {
    pub(super) events: Vec<MediaEvent>,
    pub(super) reserved_event_fits: bool,
}

impl RecorderBootstrap {
    pub(super) fn update(&mut self, event: &MediaEvent) {
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

    pub(super) fn events(&self) -> impl Iterator<Item = MediaEvent> + '_ {
        self.metadata
            .iter()
            .chain(self.aac.iter())
            .chain(self.video.iter())
            .chain(self.keyframe.iter())
            .cloned()
    }

    pub(super) fn replay_events(
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

    pub(super) fn mark_recovery_gap(
        &mut self,
        kind: MediaEventKind,
        result: RecorderEnqueueResult,
    ) {
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

    pub(super) fn finish_reap(
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

    pub(super) fn lock(&self) -> MutexGuard<'_, ControllerState> {
        self.state
            .lock()
            .expect("recorder controller mutex poisoned")
    }

    #[cfg(test)]
    pub(super) fn install_publication_gate(
        &self,
    ) -> Arc<crate::recording::recording_store::RecordingPublicationGate> {
        self.lock()
            .worker
            .as_ref()
            .expect("active recorder controller owns a worker")
            .install_publication_gate()
    }

    #[cfg(test)]
    pub(super) fn panic_on_finish(&self) {
        self.lock()
            .worker
            .as_ref()
            .expect("active recorder controller owns a worker")
            .panic_on_finish();
    }

    #[cfg(test)]
    pub(super) fn panic_after_finish(&self) {
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
