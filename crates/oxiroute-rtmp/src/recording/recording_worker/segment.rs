#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct Segment {
    muxer: FlvMuxer<CountedRecordingFile>,
    pub(super) partial_relative_name: String,
    preserve_partial: Arc<AtomicBool>,
    pub(super) partial_exists: Arc<AtomicBool>,
    frame_count: u64,
}

pub(super) struct PreparedSegment {
    file: CountedRecordingFile,
    pub(super) finalizer: crate::recording::recording_store::RecordingFinalizerAuthority,
    pub(super) partial_relative_name: String,
    pub(super) partial_exists: Arc<AtomicBool>,
}

impl Segment {
    pub(super) fn open(
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

    pub(super) fn resume(
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

    pub(super) fn write(&mut self, event: &MediaEvent) -> Result<(), WorkerError> {
        let projected = self.projected_bytes(event)?;
        write_to_muxer(&mut self.muxer, event).map(|()| {
            if projected > 0 && is_frame_event(event) {
                self.frame_count = self.frame_count.saturating_add(1);
            }
        })
    }

    pub(super) fn projected_end(&mut self, event: &MediaEvent) -> Result<Option<u64>, WorkerError> {
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

    pub(super) fn has_media(&self) -> bool {
        self.muxer.has_media()
    }

    pub(super) const fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub(super) fn preserve(&self) {
        self.preserve_partial.store(true, Ordering::Release);
    }

    pub(super) fn prepare_finish(self, final_name: String) -> Result<PreparedSegment, WorkerError> {
        self.preserve_partial.store(true, Ordering::Release);
        let mut file = self.muxer.close().map_err(|_| WorkerError {
            kind: RecorderFailure::Finalize,
            recoverable_partial_name: Some(self.partial_relative_name.clone()),
            published_but_not_durable_relative_name: None,
        })?;
        file.inner
            .set_final_relative_name(final_name)
            .map_err(|error| WorkerError::finalization(&error))?;
        let finalizer = file.inner.finalizer_authority();
        Ok(PreparedSegment {
            file,
            finalizer,
            partial_relative_name: self.partial_relative_name,
            partial_exists: self.partial_exists,
        })
    }
}

impl PreparedSegment {
    pub(super) fn finish(self) -> Result<RecordingCommit, WorkerError> {
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
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => muxer
            .write_video(event.timestamp_ms(), event.payload())
            .map_err(|_| WorkerError::new(RecorderFailure::Write)),
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
pub(super) struct SegmentHeaders {
    sequence: u64,
    metadata: Option<CachedHeader>,
    aac: Option<CachedHeader>,
    video: Option<CachedHeader>,
}

struct CachedHeader {
    sequence: u64,
    event: MediaEvent,
}

impl SegmentHeaders {
    pub(super) fn update(&mut self, event: MediaEvent) {
        self.sequence = self.sequence.saturating_add(1);
        let header = CachedHeader {
            sequence: self.sequence,
            event,
        };
        match header.event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(header),
            MediaEventKind::AacSequenceHeader => self.aac = Some(header),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => self.video = Some(header),
            _ => unreachable!("only replayable segment headers are cached"),
        }
    }

    pub(super) fn ordered(&self) -> Vec<MediaEvent> {
        let mut headers: Vec<_> = self
            .metadata
            .iter()
            .chain(self.aac.iter())
            .chain(self.video.iter())
            .collect();
        headers.sort_by_key(|header| header.sequence);
        headers
            .into_iter()
            .map(|header| header.event.clone())
            .collect()
    }
}
