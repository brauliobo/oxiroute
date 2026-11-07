#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublisherSnapshot {
    pub session_id: SessionId,
    pub attached_at_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrackSnapshot {
    pub flv_codec_id: Option<u8>,
    pub video_codec: Option<VideoCodecIdentifier>,
    pub payload_bytes_received: u64,
    pub last_rtmp_timestamp_ms: Option<u32>,
    pub last_observed_at_unix_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MediaSnapshot {
    pub audio: TrackSnapshot,
    pub video: TrackSnapshot,
    pub fanout_payload_bytes_queued: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderErrorCode {
    OpenFailed,
    WriteFailed,
    CloseFailed,
    BackendUnavailable,
    FileSyncFailed,
    PublishFailed,
    DirectorySyncFailed,
    QueueDiscontinuity,
    UnsupportedCodec,
    ShutdownTimedOut,
    WorkerPanicked,
    StalePublisher,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderPhase {
    Idle,
    Starting {
        operation_id: OperationId,
    },
    Recording {
        operation_id: OperationId,
        started_at_unix_ms: u64,
    },
    Stopping {
        operation_id: OperationId,
    },
    Failed {
        operation_id: OperationId,
        code: RecorderErrorCode,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderSnapshot {
    pub id: RecorderId,
    pub name: Option<String>,
    pub manual: bool,
    pub phase: RecorderPhase,
    pub changed_at_unix_ms: u64,
    pub bytes_written: u64,
    pub current_relative_name: Option<String>,
    pub last_completed_relative_name: Option<String>,
    pub recoverable_partial_name: Option<String>,
    pub published_but_not_durable_relative_name: Option<String>,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    pub events_enqueued: u64,
    pub events_processed: u64,
    pub events_dropped: u64,
    pub segments_started: u64,
    pub segments_completed: u64,
    pub discontinuities: u64,
    pub last_notification: Option<RecorderNotification>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelaySnapshot {
    pub id: RelayId,
    pub status: RtmpRelayStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderDefinition {
    pub name: Option<String>,
    pub manual: bool,
}

impl RecorderDefinition {
    #[must_use]
    pub fn manual(name: Option<String>) -> Self {
        Self { name, manual: true }
    }

    #[must_use]
    pub fn automatic(name: Option<String>) -> Self {
        Self {
            name,
            manual: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingAction {
    Start,
    Stop,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamSnapshot {
    pub id: StreamId,
    pub revision: u64,
    pub key: StreamKey,
    pub created_at_unix_ms: u64,
    pub publisher: Option<PublisherSnapshot>,
    pub subscriber_count: usize,
    pub media: MediaSnapshot,
    pub relays: Vec<RelaySnapshot>,
    pub recorders: Vec<RecorderSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpCatalogSnapshot {
    pub revision: u64,
    pub as_of_unix_ms: u64,
    pub capabilities: RtmpCapabilities,
    pub streams: Vec<StreamSnapshot>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RtmpRegistryWorkStats {
    pub media_updates: u64,
    pub snapshot_rebuilds: u64,
    pub snapshot_streams_visited: u64,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum CatalogError {
    #[error("RTMP runtime admission is closed")]
    AdmissionClosed,
    #[error("stream {0} does not exist")]
    StreamNotFound(StreamId),
    #[error("stream {stream_id} already has publisher {session_id}")]
    PublisherAlreadyAttached {
        stream_id: StreamId,
        session_id: SessionId,
    },
    #[error("publisher {session_id} is not attached to stream {stream_id}")]
    PublisherMismatch {
        stream_id: StreamId,
        session_id: SessionId,
    },
    #[error("subscriber {session_id} is not attached to stream {stream_id}")]
    SubscriberNotFound {
        stream_id: StreamId,
        session_id: SessionId,
    },
    #[error("recorder {recorder_id} does not exist on stream {stream_id}")]
    RecorderNotFound {
        stream_id: StreamId,
        recorder_id: RecorderId,
    },
    #[error("RTMP recording is unavailable in the active runtime")]
    RecordingUnavailable,
    #[error("recorder {0} is not manual")]
    RecorderNotManual(RecorderId),
    #[error("stream {0} has no active publisher")]
    NoPublisher(StreamId),
    #[error("recorder {0} has an opposite transition in progress")]
    TransitionInProgress(RecorderId),
    #[error("recorder operation is stale")]
    StaleOperation,
    #[error("recorder completion does not match the active transition")]
    InvalidCompletion,
    #[error("recorder {recorder_id} failed with {code:?}")]
    RecorderFailed {
        recorder_id: RecorderId,
        code: RecorderErrorCode,
    },
}
