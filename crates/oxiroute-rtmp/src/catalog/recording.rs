#[allow(clippy::wildcard_imports)]
use super::*;

impl RtmpRegistry {
    /// Requests an idempotent manual recorder transition.
    ///
    /// # Errors
    ///
    /// Returns an error when recording is unavailable, the target is stale, or an opposite
    /// transition is still in progress.
    fn request_recording_transition(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        action: RecordingAction,
        at_unix_ms: u64,
    ) -> Result<RecorderSnapshot, CatalogError> {
        if !self.capabilities.manual_recording {
            return Err(CatalogError::RecordingUnavailable);
        }

        let mut inner = self.lock();
        if action == RecordingAction::Start && !inner.admission_open {
            return Err(CatalogError::AdmissionClosed);
        }
        refresh_recorder_statuses(&mut inner);
        let (snapshot, changed) = {
            let stream = inner
                .streams
                .get_mut(&stream_id)
                .ok_or(CatalogError::StreamNotFound(stream_id))?;
            if stream.publisher.is_none() {
                return Err(CatalogError::NoPublisher(stream_id));
            }
            let recorder =
                stream
                    .recorders
                    .get_mut(&recorder_id)
                    .ok_or(CatalogError::RecorderNotFound {
                        stream_id,
                        recorder_id,
                    })?;
            if !recorder.manual {
                return Err(CatalogError::RecorderNotManual(recorder_id));
            }
            if recorder.control.is_none() {
                return Err(CatalogError::RecordingUnavailable);
            }

            let changed = match (action, recorder.phase) {
                (RecordingAction::Start, RecorderPhase::Idle | RecorderPhase::Failed { .. }) => {
                    recorder.phase = RecorderPhase::Starting {
                        operation_id: OperationId::new(),
                    };
                    true
                }
                (
                    RecordingAction::Start,
                    RecorderPhase::Starting { .. } | RecorderPhase::Recording { .. },
                )
                | (
                    RecordingAction::Stop,
                    RecorderPhase::Idle
                    | RecorderPhase::Failed { .. }
                    | RecorderPhase::Stopping { .. },
                ) => false,
                (RecordingAction::Stop, RecorderPhase::Recording { .. }) => {
                    recorder.phase = RecorderPhase::Stopping {
                        operation_id: OperationId::new(),
                    };
                    true
                }
                (RecordingAction::Start, RecorderPhase::Stopping { .. })
                | (RecordingAction::Stop, RecorderPhase::Starting { .. }) => {
                    return Err(CatalogError::TransitionInProgress(recorder_id));
                }
            };
            if changed {
                recorder.changed_at_unix_ms = at_unix_ms;
                stream.revision = stream.revision.saturating_add(1);
            }
            (recorder.snapshot(), changed)
        };
        if changed {
            mark_mutation(&mut inner, at_unix_ms);
            publish_snapshot_if_dirty(&mut inner, self.capabilities);
        }
        Ok(snapshot)
    }

    /// Starts one exact manual recorder owned by the current publisher incarnation.
    ///
    /// # Errors
    ///
    /// Returns stable capability, not-found, conflict, stale-publisher, or recorder-failure errors.
    pub fn start_recording(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) -> Result<RecorderSnapshot, CatalogError> {
        let snapshot = self.request_recording_transition(
            stream_id,
            recorder_id,
            RecordingAction::Start,
            at_unix_ms,
        )?;
        if !matches!(snapshot.phase, RecorderPhase::Starting { .. }) {
            return Ok(snapshot);
        }
        self.execute_start(stream_id, recorder_id, at_unix_ms)
    }

    /// Stops one exact manual recorder owned by the current publisher incarnation.
    ///
    /// # Errors
    ///
    /// Returns stable capability, not-found, conflict, stale-publisher, or recorder-failure errors.
    pub fn stop_recording(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) -> Result<RecorderSnapshot, CatalogError> {
        let snapshot = self.request_recording_transition(
            stream_id,
            recorder_id,
            RecordingAction::Stop,
            at_unix_ms,
        )?;
        if !matches!(snapshot.phase, RecorderPhase::Stopping { .. }) {
            return Ok(snapshot);
        }
        let (control, context) = self.command_context(stream_id, recorder_id, at_unix_ms)?;
        if control.stop(context) {
            return self.recorder_snapshot(stream_id, recorder_id);
        }
        self.fail_recorder(context, RecorderErrorCode::BackendUnavailable);
        Err(CatalogError::RecorderFailed {
            recorder_id,
            code: RecorderErrorCode::BackendUnavailable,
        })
    }

    pub(crate) fn start_continuous_recording(
        &self,
        stream_id: StreamId,
        publisher_session_id: SessionId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) {
        let operation_id = OperationId::new();
        {
            let mut inner = self.lock();
            if !inner.admission_open {
                return;
            }
            let Some(stream) = inner.streams.get_mut(&stream_id) else {
                return;
            };
            if stream.publisher.map(|publisher| publisher.session_id) != Some(publisher_session_id)
            {
                return;
            }
            let Some(recorder) = stream.recorders.get_mut(&recorder_id) else {
                return;
            };
            if recorder.manual {
                return;
            }
            recorder.phase = RecorderPhase::Starting { operation_id };
            recorder.changed_at_unix_ms = recorder.changed_at_unix_ms.max(at_unix_ms);
            stream.revision = stream.revision.saturating_add(1);
            mark_mutation(&mut inner, at_unix_ms);
        }
        self.execute_continuous_start(stream_id, recorder_id, at_unix_ms);
    }

    pub(crate) fn update_recorder_runtime(
        &self,
        stream_id: StreamId,
        publisher_session_id: SessionId,
        recorder_id: RecorderId,
        enqueue_result: RecorderEnqueueResult,
        at_unix_ms: u64,
    ) {
        let mut inner = self.lock();
        let Some(stream) = inner.streams.get_mut(&stream_id) else {
            return;
        };
        if stream.publisher.map(|publisher| publisher.session_id) != Some(publisher_session_id) {
            return;
        }
        let Some(recorder) = stream.recorders.get_mut(&recorder_id) else {
            return;
        };
        let mut changed = recorder
            .control
            .clone()
            .is_some_and(|control| apply_runtime_status(recorder, control.status()));
        if enqueue_result == RecorderEnqueueResult::DroppedDiscontinuity
            && let Some(operation_id) = active_operation(recorder.phase)
        {
            let phase = RecorderPhase::Failed {
                operation_id,
                code: RecorderErrorCode::QueueDiscontinuity,
            };
            if recorder.phase != phase {
                recorder.phase = phase;
                changed = true;
            }
        }
        if changed {
            recorder.changed_at_unix_ms = recorder.changed_at_unix_ms.max(at_unix_ms);
            stream.revision = stream.revision.saturating_add(1);
            mark_mutation(&mut inner, at_unix_ms);
        }
    }

    pub(crate) fn complete_worker_stop(
        &self,
        context: RecorderCommandContext,
        status: &RecorderWorkerStatus,
    ) {
        let mut inner = self.lock();
        let Some(stream) = inner.streams.get_mut(&context.stream_id) else {
            return;
        };
        if stream.publisher.map(|publisher| publisher.session_id)
            != Some(context.publisher_session_id)
        {
            return;
        }
        let Some(recorder) = stream.recorders.get_mut(&context.recorder_id) else {
            return;
        };
        if !matches!(
            recorder.phase,
            RecorderPhase::Stopping { operation_id } if operation_id == context.operation_id
        ) {
            return;
        }
        apply_worker_details(recorder, status);
        recorder.phase = match status.phase {
            RecorderWorkerPhase::Failed(failure) => RecorderPhase::Failed {
                operation_id: context.operation_id,
                code: recorder_error_code(failure),
            },
            RecorderWorkerPhase::Stopped => RecorderPhase::Idle,
            RecorderWorkerPhase::Starting | RecorderWorkerPhase::Recording => {
                RecorderPhase::Failed {
                    operation_id: context.operation_id,
                    code: RecorderErrorCode::BackendUnavailable,
                }
            }
        };
        recorder.changed_at_unix_ms = recorder.changed_at_unix_ms.max(context.at_unix_ms);
        stream.revision = stream.revision.saturating_add(1);
        mark_mutation(&mut inner, context.at_unix_ms);
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner.lock().expect("RTMP registry mutex poisoned")
    }

    pub(super) fn recorder_ids(
        &self,
        stream_id: StreamId,
        publisher_session_id: SessionId,
    ) -> Result<Vec<RecorderId>, CatalogError> {
        let inner = self.lock();
        let stream = inner
            .streams
            .get(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        if stream.publisher.map(|publisher| publisher.session_id) != Some(publisher_session_id) {
            return Err(CatalogError::PublisherMismatch {
                stream_id,
                session_id: publisher_session_id,
            });
        }
        Ok(stream.recorder_order.clone())
    }

    fn command_context(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) -> Result<(Arc<RecorderController>, RecorderCommandContext), CatalogError> {
        let inner = self.lock();
        let stream = inner
            .streams
            .get(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        let publisher_session_id = stream
            .publisher
            .map(|publisher| publisher.session_id)
            .ok_or(CatalogError::NoPublisher(stream_id))?;
        let recorder =
            stream
                .recorders
                .get(&recorder_id)
                .ok_or(CatalogError::RecorderNotFound {
                    stream_id,
                    recorder_id,
                })?;
        let operation_id =
            active_operation(recorder.phase).ok_or(CatalogError::InvalidCompletion)?;
        let control = recorder
            .control
            .clone()
            .ok_or(CatalogError::RecordingUnavailable)?;
        Ok((
            control,
            RecorderCommandContext {
                stream_id,
                publisher_session_id,
                recorder_id,
                operation_id,
                at_unix_ms,
            },
        ))
    }

    fn execute_start(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) -> Result<RecorderSnapshot, CatalogError> {
        let (control, context) = self.command_context(stream_id, recorder_id, at_unix_ms)?;
        if let Err(code) = control.start(context) {
            self.fail_recorder(context, code);
            return Err(CatalogError::RecorderFailed { recorder_id, code });
        }
        self.mark_recorder_started(context);
        self.recorder_snapshot(stream_id, recorder_id)
    }

    fn execute_continuous_start(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) {
        let Ok((control, context)) = self.command_context(stream_id, recorder_id, at_unix_ms)
        else {
            return;
        };
        match control.start_continuous(context) {
            Ok(()) => self.mark_recorder_started(context),
            Err(RecorderStartFailure::RetryableCapacity) => {}
            Err(RecorderStartFailure::Failed(code)) => {
                self.fail_recorder(context, code);
            }
        }
    }

    fn mark_recorder_started(&self, context: RecorderCommandContext) {
        let mut inner = self.lock();
        let Some(stream) = inner.streams.get_mut(&context.stream_id) else {
            return;
        };
        if stream.publisher.map(|publisher| publisher.session_id)
            != Some(context.publisher_session_id)
        {
            return;
        }
        let Some(recorder) = stream.recorders.get_mut(&context.recorder_id) else {
            return;
        };
        if !matches!(
            recorder.phase,
            RecorderPhase::Starting { operation_id } if operation_id == context.operation_id
        ) {
            return;
        }
        recorder.phase = RecorderPhase::Recording {
            operation_id: context.operation_id,
            started_at_unix_ms: context.at_unix_ms,
        };
        recorder.changed_at_unix_ms = recorder.changed_at_unix_ms.max(context.at_unix_ms);
        stream.revision = stream.revision.saturating_add(1);
        mark_mutation(&mut inner, context.at_unix_ms);
    }

    fn fail_recorder(&self, context: RecorderCommandContext, code: RecorderErrorCode) {
        let mut inner = self.lock();
        let Some(stream) = inner.streams.get_mut(&context.stream_id) else {
            return;
        };
        if stream.publisher.map(|publisher| publisher.session_id)
            != Some(context.publisher_session_id)
        {
            return;
        }
        let Some(recorder) = stream.recorders.get_mut(&context.recorder_id) else {
            return;
        };
        if active_operation(recorder.phase) != Some(context.operation_id) {
            return;
        }
        recorder.phase = RecorderPhase::Failed {
            operation_id: context.operation_id,
            code,
        };
        recorder.changed_at_unix_ms = recorder.changed_at_unix_ms.max(context.at_unix_ms);
        stream.revision = stream.revision.saturating_add(1);
        mark_mutation(&mut inner, context.at_unix_ms);
    }

    pub(super) fn recorder_snapshot(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
    ) -> Result<RecorderSnapshot, CatalogError> {
        let mut inner = self.lock();
        refresh_recorder_statuses(&mut inner);
        inner
            .streams
            .get(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?
            .recorders
            .get(&recorder_id)
            .map(MutableRecorder::snapshot)
            .ok_or(CatalogError::RecorderNotFound {
                stream_id,
                recorder_id,
            })
    }

    pub(crate) fn has_publisher(&self, key: &StreamKey) -> bool {
        let inner = self.lock();
        inner
            .streams_by_key
            .get(key)
            .and_then(|stream_id| inner.streams.get(stream_id))
            .is_some_and(|stream| stream.publisher.is_some())
    }

    pub(crate) fn initiate_recorder_shutdown(&self, reaper: &RecorderReaperHandle) {
        let commands = {
            let mut inner = self.lock();
            let mut commands = Vec::new();
            let mut observed_at_unix_ms = inner.as_of_unix_ms;
            let mut changed = false;
            for stream in inner.streams.values_mut() {
                let Some(publisher) = stream.publisher else {
                    continue;
                };
                let mut stream_changed = false;
                for recorder in stream.recorders.values_mut() {
                    let Some(control) = recorder.control.clone() else {
                        continue;
                    };
                    if !control.uses_reaper(reaper)
                        || matches!(
                            recorder.phase,
                            RecorderPhase::Idle | RecorderPhase::Stopping { .. }
                        )
                    {
                        continue;
                    }
                    let Some(operation_id) = active_operation(recorder.phase) else {
                        continue;
                    };
                    let at_unix_ms = control.observed_at_unix_ms();
                    recorder.phase = RecorderPhase::Stopping { operation_id };
                    recorder.changed_at_unix_ms = recorder.changed_at_unix_ms.max(at_unix_ms);
                    observed_at_unix_ms = observed_at_unix_ms.max(at_unix_ms);
                    stream_changed = true;
                    commands.push((
                        control,
                        RecorderCommandContext {
                            stream_id: stream.id,
                            publisher_session_id: publisher.session_id,
                            recorder_id: recorder.id,
                            operation_id,
                            at_unix_ms,
                        },
                    ));
                }
                if stream_changed {
                    stream.revision = stream.revision.saturating_add(1);
                    changed = true;
                }
            }
            if changed {
                mark_mutation(&mut inner, observed_at_unix_ms);
            }
            commands
        };
        for (control, context) in commands {
            if !control.stop(context) {
                self.fail_recorder(context, RecorderErrorCode::BackendUnavailable);
            }
        }
    }
}

fn active_operation(phase: RecorderPhase) -> Option<OperationId> {
    match phase {
        RecorderPhase::Starting { operation_id }
        | RecorderPhase::Recording { operation_id, .. }
        | RecorderPhase::Stopping { operation_id }
        | RecorderPhase::Failed { operation_id, .. } => Some(operation_id),
        RecorderPhase::Idle => None,
    }
}

pub(super) fn refresh_recorder_statuses(inner: &mut RegistryInner) {
    let mut changed = false;
    let mut observed_at_unix_ms = inner.as_of_unix_ms;
    for stream in inner.streams.values_mut() {
        let mut stream_changed = false;
        for recorder in stream.recorders.values_mut() {
            let Some(control) = recorder.control.clone() else {
                continue;
            };
            let runtime = control.status();
            observed_at_unix_ms = observed_at_unix_ms.max(runtime.observed_at_unix_ms);
            let recorder_observed_at_unix_ms = runtime.observed_at_unix_ms;
            if apply_runtime_status(recorder, runtime) {
                recorder.changed_at_unix_ms = recorder
                    .changed_at_unix_ms
                    .max(recorder_observed_at_unix_ms);
                stream_changed = true;
            }
        }
        if stream_changed {
            stream.revision = stream.revision.saturating_add(1);
            changed = true;
        }
    }
    if changed {
        mark_mutation(inner, observed_at_unix_ms);
    }
}

pub(super) fn apply_runtime_status(
    recorder: &mut MutableRecorder,
    runtime: RecorderRuntimeStatus,
) -> bool {
    let before = recorder.snapshot();
    let Some(status) = runtime.status else {
        recorder.events_dropped = runtime.recovery_events_dropped;
        return recorder.snapshot() != before;
    };
    let replacement_worker = runtime.worker_generation > recorder.worker_generation;
    apply_worker_details(recorder, &status);
    if runtime.recovering
        && let (RecorderWorkerPhase::Failed(failure), Some(operation_id)) =
            (status.phase, active_operation(recorder.phase))
    {
        recorder.phase = RecorderPhase::Failed {
            operation_id,
            code: recorder_error_code(failure),
        };
    }
    if !runtime.stopping {
        let operation_id = active_operation(recorder.phase);
        recorder.phase = match (status.phase, operation_id, recorder.phase) {
            (
                RecorderWorkerPhase::Recording,
                Some(operation_id),
                RecorderPhase::Starting { .. },
            ) => RecorderPhase::Recording {
                operation_id,
                started_at_unix_ms: runtime.observed_at_unix_ms,
            },
            (RecorderWorkerPhase::Failed(failure), Some(operation_id), _) => {
                RecorderPhase::Failed {
                    operation_id,
                    code: recorder_error_code(failure),
                }
            }
            (RecorderWorkerPhase::Stopped, Some(_), RecorderPhase::Stopping { .. }) => {
                RecorderPhase::Idle
            }
            (RecorderWorkerPhase::Starting, Some(operation_id), RecorderPhase::Failed { .. })
                if replacement_worker =>
            {
                RecorderPhase::Starting { operation_id }
            }
            (RecorderWorkerPhase::Recording, Some(operation_id), RecorderPhase::Failed { .. })
                if replacement_worker =>
            {
                RecorderPhase::Recording {
                    operation_id,
                    started_at_unix_ms: runtime.observed_at_unix_ms,
                }
            }
            _ => recorder.phase,
        };
    }
    recorder.worker_generation = recorder.worker_generation.max(runtime.worker_generation);
    recorder.snapshot() != before
}

fn apply_worker_details(recorder: &mut MutableRecorder, status: &RecorderWorkerStatus) {
    recorder.bytes_written = status.bytes_written;
    recorder
        .current_relative_name
        .clone_from(&status.current_relative_name);
    recorder
        .last_completed_relative_name
        .clone_from(&status.last_completed_relative_name);
    recorder
        .recoverable_partial_name
        .clone_from(&status.recoverable_partial_name);
    recorder
        .published_but_not_durable_relative_name
        .clone_from(&status.published_but_not_durable_relative_name);
    recorder.queue_messages = status.queue_messages;
    recorder.queue_bytes = status.queue_bytes;
    recorder.events_enqueued = status.events_enqueued;
    recorder.events_processed = status.events_processed;
    recorder.events_dropped = status.events_dropped;
    recorder.segments_started = status.segments_started;
    recorder.segments_completed = status.segments_completed;
    recorder.discontinuities = status.discontinuities;
    recorder.last_notification = status.last_notification;
}
