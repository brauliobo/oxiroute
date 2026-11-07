#[allow(clippy::wildcard_imports)]
use super::*;

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

pub(super) struct WorkerSetup {
    pub(super) store: RecordingStore,
    pub(super) path_policy: RecordingPathPolicy,
    pub(super) stream_name: Arc<[u8]>,
    pub(super) lease: RecorderLease,
    pub(super) rotation_interval: Option<Duration>,
}

pub(super) fn run_worker(shared: &Arc<WorkerShared>, setup: WorkerSetup) {
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
            if let Some(segment) = &mut self.segment {
                segment.write(event)?;
            } else {
                self.open_segment(queued.arrived_at, queued.arrived_at_unix_seconds)?;
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
        let finalizer = segment.finalizer.clone();
        let ticket = finalizer.submit(Box::new(move || {
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
        MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable
    ) && event
        .video_codec_identifier()
        .is_some_and(|identifier| !identifier.recording_supported())
}

pub(super) fn is_frame_event(event: &MediaEvent) -> bool {
    matches!(
        event.kind(),
        MediaEventKind::Audio
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable
    )
}
