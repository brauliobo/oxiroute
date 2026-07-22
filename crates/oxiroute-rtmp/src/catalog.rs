use std::{
    collections::{HashMap, HashSet},
    fmt::{Display, Formatter},
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
};

use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

id_type!(StreamId);
id_type!(SessionId);
id_type!(RecorderId);
id_type!(OperationId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpCapabilities {
    pub live_ingest: bool,
    pub manual_recording: bool,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StreamKey {
    pub server_id: String,
    pub application: String,
    pub name: String,
}

impl StreamKey {
    #[must_use]
    pub fn new(
        server_id: impl Into<String>,
        application: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            application: application.into(),
            name: name.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublisherSnapshot {
    pub session_id: SessionId,
    pub attached_at_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrackSnapshot {
    pub flv_codec_id: Option<u8>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderCompletion {
    Started,
    Stopped,
    Failed(RecorderErrorCode),
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
    pub recorders: Vec<RecorderSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpCatalogSnapshot {
    pub revision: u64,
    pub as_of_unix_ms: u64,
    pub capabilities: RtmpCapabilities,
    pub streams: Vec<StreamSnapshot>,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum CatalogError {
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
    #[error("manual RTMP recording is not implemented by the active runtime")]
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
}

struct MutableRecorder {
    id: RecorderId,
    name: Option<String>,
    manual: bool,
    phase: RecorderPhase,
    changed_at_unix_ms: u64,
    bytes_written: u64,
}

impl MutableRecorder {
    fn snapshot(&self) -> RecorderSnapshot {
        RecorderSnapshot {
            id: self.id,
            name: self.name.clone(),
            manual: self.manual,
            phase: self.phase,
            changed_at_unix_ms: self.changed_at_unix_ms,
            bytes_written: self.bytes_written,
        }
    }
}

struct MutableStream {
    id: StreamId,
    revision: u64,
    key: StreamKey,
    created_at_unix_ms: u64,
    publisher: Option<PublisherSnapshot>,
    subscribers: HashSet<SessionId>,
    media: MediaSnapshot,
    media_sample_sequence: u64,
    recorders: HashMap<RecorderId, MutableRecorder>,
}

impl MutableStream {
    fn new(key: StreamKey, at_unix_ms: u64) -> Self {
        Self {
            id: StreamId::new(),
            revision: 0,
            key,
            created_at_unix_ms: at_unix_ms,
            publisher: None,
            subscribers: HashSet::new(),
            media: MediaSnapshot::default(),
            media_sample_sequence: 0,
            recorders: HashMap::new(),
        }
    }

    fn snapshot(&self) -> StreamSnapshot {
        let mut recorders: Vec<_> = self
            .recorders
            .values()
            .map(MutableRecorder::snapshot)
            .collect();
        recorders.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));

        StreamSnapshot {
            id: self.id,
            revision: self.revision,
            key: self.key.clone(),
            created_at_unix_ms: self.created_at_unix_ms,
            publisher: self.publisher,
            subscriber_count: self.subscribers.len(),
            media: self.media,
            recorders,
        }
    }
}

struct RegistryInner {
    revision: u64,
    streams: HashMap<StreamId, MutableStream>,
    streams_by_key: HashMap<StreamKey, StreamId>,
    current: Arc<RtmpCatalogSnapshot>,
}

pub struct RtmpRegistry {
    capabilities: RtmpCapabilities,
    inner: Mutex<RegistryInner>,
}

impl RtmpRegistry {
    #[must_use]
    pub fn new(capabilities: RtmpCapabilities) -> Self {
        Self {
            capabilities,
            inner: Mutex::new(RegistryInner {
                revision: 0,
                streams: HashMap::new(),
                streams_by_key: HashMap::new(),
                current: Arc::new(RtmpCatalogSnapshot {
                    revision: 0,
                    as_of_unix_ms: 0,
                    capabilities,
                    streams: Vec::new(),
                }),
            }),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Arc<RtmpCatalogSnapshot> {
        Arc::clone(&self.lock().current)
    }

    /// Attaches a publisher to a logical stream.
    ///
    /// # Errors
    ///
    /// Returns an error when another publisher already owns the stream.
    pub fn attach_publisher(
        &self,
        key: StreamKey,
        session_id: SessionId,
        recorder_definitions: Vec<RecorderDefinition>,
        at_unix_ms: u64,
    ) -> Result<StreamId, CatalogError> {
        let mut inner = self.lock();
        let stream_id = ensure_stream(&mut inner, key, at_unix_ms);
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;

        if let Some(publisher) = stream.publisher {
            if publisher.session_id == session_id {
                return Ok(stream_id);
            }
            return Err(CatalogError::PublisherAlreadyAttached {
                stream_id,
                session_id: publisher.session_id,
            });
        }

        stream.publisher = Some(PublisherSnapshot {
            session_id,
            attached_at_unix_ms: at_unix_ms,
        });
        stream.media = MediaSnapshot::default();
        stream.media_sample_sequence = 0;
        stream.recorders = recorder_definitions
            .into_iter()
            .map(|definition| {
                let id = RecorderId::new();
                (
                    id,
                    MutableRecorder {
                        id,
                        name: definition.name,
                        manual: definition.manual,
                        phase: RecorderPhase::Idle,
                        changed_at_unix_ms: at_unix_ms,
                        bytes_written: 0,
                    },
                )
            })
            .collect();
        stream.revision += 1;
        rebuild_snapshot(&mut inner, self.capabilities, at_unix_ms);
        Ok(stream_id)
    }

    /// Publishes one absolute media statistics sample for the current publisher.
    ///
    /// Returns `false` when a delayed sample sequence has already been superseded.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing stream or stale publisher session.
    pub fn update_media_sample(
        &self,
        stream_id: StreamId,
        publisher_session_id: SessionId,
        sequence: u64,
        media: MediaSnapshot,
        at_unix_ms: u64,
    ) -> Result<bool, CatalogError> {
        let mut inner = self.lock();
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        if stream.publisher.map(|publisher| publisher.session_id) != Some(publisher_session_id) {
            return Err(CatalogError::PublisherMismatch {
                stream_id,
                session_id: publisher_session_id,
            });
        }
        if sequence <= stream.media_sample_sequence {
            return Ok(false);
        }

        stream.media_sample_sequence = sequence;
        stream.media = media;
        stream.revision += 1;
        rebuild_snapshot(&mut inner, self.capabilities, at_unix_ms);
        Ok(true)
    }

    /// Attaches a subscriber to a logical stream.
    ///
    /// # Errors
    ///
    /// This operation currently has no expected error state.
    pub fn attach_subscriber(
        &self,
        key: StreamKey,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<StreamId, CatalogError> {
        let mut inner = self.lock();
        let stream_id = ensure_stream(&mut inner, key, at_unix_ms);
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        if stream.subscribers.insert(session_id) {
            stream.revision += 1;
            rebuild_snapshot(&mut inner, self.capabilities, at_unix_ms);
        }
        Ok(stream_id)
    }

    /// Detaches the current publisher.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing stream or stale publisher session.
    pub fn detach_publisher(
        &self,
        stream_id: StreamId,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<(), CatalogError> {
        let mut inner = self.lock();
        let remove = {
            let stream = inner
                .streams
                .get_mut(&stream_id)
                .ok_or(CatalogError::StreamNotFound(stream_id))?;
            if stream.publisher.map(|publisher| publisher.session_id) != Some(session_id) {
                return Err(CatalogError::PublisherMismatch {
                    stream_id,
                    session_id,
                });
            }
            stream.publisher = None;
            stream.media = MediaSnapshot::default();
            stream.media_sample_sequence = 0;
            stream.recorders.clear();
            stream.revision += 1;
            stream.subscribers.is_empty()
        };
        if remove {
            remove_stream(&mut inner, stream_id);
        }
        rebuild_snapshot(&mut inner, self.capabilities, at_unix_ms);
        Ok(())
    }

    /// Detaches a subscriber.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing stream or stale subscriber session.
    pub fn detach_subscriber(
        &self,
        stream_id: StreamId,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<(), CatalogError> {
        let mut inner = self.lock();
        let remove = {
            let stream = inner
                .streams
                .get_mut(&stream_id)
                .ok_or(CatalogError::StreamNotFound(stream_id))?;
            if !stream.subscribers.remove(&session_id) {
                return Err(CatalogError::SubscriberNotFound {
                    stream_id,
                    session_id,
                });
            }
            stream.revision += 1;
            stream.publisher.is_none() && stream.subscribers.is_empty()
        };
        if remove {
            remove_stream(&mut inner, stream_id);
        }
        rebuild_snapshot(&mut inner, self.capabilities, at_unix_ms);
        Ok(())
    }

    /// Requests an idempotent manual recorder transition.
    ///
    /// # Errors
    ///
    /// Returns an error when recording is unavailable, the target is stale, or an opposite
    /// transition is still in progress.
    pub fn request_recording(
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
                stream.revision += 1;
            }
            (recorder.snapshot(), changed)
        };
        if changed {
            rebuild_snapshot(&mut inner, self.capabilities, at_unix_ms);
        }
        Ok(snapshot)
    }

    /// Applies a publisher-side recorder completion carrying all incarnation tokens.
    ///
    /// # Errors
    ///
    /// Returns an error for stale identities or a completion that does not match the active
    /// transition.
    pub fn complete_recording(
        &self,
        stream_id: StreamId,
        publisher_session_id: SessionId,
        recorder_id: RecorderId,
        operation_id: OperationId,
        completion: RecorderCompletion,
        at_unix_ms: u64,
    ) -> Result<RecorderSnapshot, CatalogError> {
        let mut inner = self.lock();
        let snapshot = {
            let stream = inner
                .streams
                .get_mut(&stream_id)
                .ok_or(CatalogError::StreamNotFound(stream_id))?;
            if stream.publisher.map(|publisher| publisher.session_id) != Some(publisher_session_id)
            {
                return Err(CatalogError::PublisherMismatch {
                    stream_id,
                    session_id: publisher_session_id,
                });
            }
            let recorder =
                stream
                    .recorders
                    .get_mut(&recorder_id)
                    .ok_or(CatalogError::RecorderNotFound {
                        stream_id,
                        recorder_id,
                    })?;
            let active_operation = match recorder.phase {
                RecorderPhase::Starting { operation_id }
                | RecorderPhase::Recording { operation_id, .. }
                | RecorderPhase::Stopping { operation_id }
                | RecorderPhase::Failed { operation_id, .. } => Some(operation_id),
                RecorderPhase::Idle => None,
            };
            if active_operation != Some(operation_id) {
                return Err(CatalogError::StaleOperation);
            }

            recorder.phase = match (recorder.phase, completion) {
                (RecorderPhase::Starting { .. }, RecorderCompletion::Started) => {
                    RecorderPhase::Recording {
                        operation_id,
                        started_at_unix_ms: at_unix_ms,
                    }
                }
                (RecorderPhase::Stopping { .. }, RecorderCompletion::Stopped) => {
                    RecorderPhase::Idle
                }
                (
                    RecorderPhase::Starting { .. } | RecorderPhase::Stopping { .. },
                    RecorderCompletion::Failed(code),
                ) => RecorderPhase::Failed { operation_id, code },
                _ => return Err(CatalogError::InvalidCompletion),
            };
            recorder.changed_at_unix_ms = at_unix_ms;
            stream.revision += 1;
            recorder.snapshot()
        };
        rebuild_snapshot(&mut inner, self.capabilities, at_unix_ms);
        Ok(snapshot)
    }

    fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner.lock().expect("RTMP registry mutex poisoned")
    }
}

fn ensure_stream(inner: &mut RegistryInner, key: StreamKey, at_unix_ms: u64) -> StreamId {
    if let Some(stream_id) = inner.streams_by_key.get(&key) {
        return *stream_id;
    }

    let stream = MutableStream::new(key.clone(), at_unix_ms);
    let stream_id = stream.id;
    inner.streams_by_key.insert(key, stream_id);
    inner.streams.insert(stream_id, stream);
    stream_id
}

fn remove_stream(inner: &mut RegistryInner, stream_id: StreamId) {
    if let Some(stream) = inner.streams.remove(&stream_id) {
        inner.streams_by_key.remove(&stream.key);
    }
}

fn rebuild_snapshot(inner: &mut RegistryInner, capabilities: RtmpCapabilities, at_unix_ms: u64) {
    inner.revision += 1;
    let mut streams: Vec<_> = inner
        .streams
        .values()
        .map(MutableStream::snapshot)
        .collect();
    streams.sort_by(|left, right| left.key.cmp(&right.key).then(left.id.cmp(&right.id)));
    inner.current = Arc::new(RtmpCatalogSnapshot {
        revision: inner.revision,
        as_of_unix_ms: at_unix_ms,
        capabilities,
        streams,
    });
}
