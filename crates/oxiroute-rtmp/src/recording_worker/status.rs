use std::io;

use crate::{RecordingPathError, RecordingStoreError};

use super::{MAX_ROTATION_INTERVAL_MS, RecorderVideoCodec};

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
