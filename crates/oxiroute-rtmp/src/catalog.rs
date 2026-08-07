use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
};

use uuid::Uuid;

use crate::{
    RecorderEnqueueResult, RecorderNotification, RecorderWorkerPhase, RecorderWorkerStatus,
    live::VideoCodecIdentifier,
    recording_runtime::{
        RecorderCommandContext, RecorderController, RecorderReaper, RecorderReaperHandle,
        RecorderReaperOwner, RecorderRuntimeStatus, RecorderStartFailure, recorder_error_code,
    },
    relay::{RtmpRelayController, RtmpRelayStatus},
    session_control::{
        MAX_RTMP_SESSION_CONTROLS, RtmpClientSnapshot, RtmpSessionControl,
        RtmpSessionControlAction, RtmpSessionControlError, RtmpSessionControlOutcome,
    },
};

pub const MAX_RTMP_APPLICATION_BYTES: usize = 128;
pub const MAX_RTMP_STREAM_NAME_BYTES: usize = 512;
pub const MAX_RTMP_QUERY_BYTES: usize = 1024;

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
id_type!(RelayId);
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

/// Parsed RTMP URL path with authentication/query data kept separate from stream identity.
#[derive(Clone, Eq, PartialEq)]
pub struct RtmpStreamPath {
    application: String,
    stream_name: String,
    query: Option<String>,
}

impl std::fmt::Debug for RtmpStreamPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtmpStreamPath")
            .field("application", &self.application)
            .field("stream_name", &self.stream_name)
            .field("query", &self.query.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl RtmpStreamPath {
    /// Parses the application and stream components supplied by the RTMP protocol.
    ///
    /// Applications and stream names are single path components. A nonempty stream query is
    /// retained as protocol data but deliberately omitted from [`Self::stream_key`].
    ///
    /// # Errors
    ///
    /// Returns an error for empty, nested, fragmented, control-character, or ambiguous paths.
    pub fn parse(application: &str, protocol_name: &str) -> Result<Self, RtmpStreamPathError> {
        let (application, stream_name, query) = validated_rtmp_path(application, protocol_name)?;

        Ok(Self {
            application: application.to_owned(),
            stream_name: stream_name.to_owned(),
            query: query.map(str::to_owned),
        })
    }

    #[must_use]
    pub fn application(&self) -> &str {
        &self.application
    }

    #[must_use]
    pub fn stream_name(&self) -> &str {
        &self.stream_name
    }

    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    #[must_use]
    pub fn stream_key(&self, service_id: impl Into<String>) -> StreamKey {
        StreamKey::new(service_id, &self.application, &self.stream_name)
    }

    #[must_use]
    pub fn into_stream_key(self, service_id: impl Into<String>) -> StreamKey {
        StreamKey::new(service_id, self.application, self.stream_name)
    }

    /// Validates an RTMP connection application before any application-owned clone is made.
    ///
    /// # Errors
    ///
    /// Returns a stable component or byte-limit error.
    pub fn validate_application(application: &str) -> Result<(), RtmpStreamPathError> {
        validate_application(application)
    }

    pub(crate) fn matches_key(key: &StreamKey, application: &str, protocol_name: &str) -> bool {
        validated_rtmp_path(application, protocol_name).is_ok_and(
            |(application, stream_name, _)| {
                key.application == application && key.name == stream_name
            },
        )
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum RtmpStreamPathError {
    #[error("RTMP application must be one nonempty path component")]
    Application,
    #[error("RTMP stream name must be one nonempty path component")]
    StreamName,
    #[error("RTMP stream query must be nonempty and contain no fragment or control characters")]
    Query,
    #[error("RTMP application is {size} bytes; maximum is {maximum} bytes")]
    ApplicationTooLong { size: usize, maximum: usize },
    #[error("RTMP stream name is {size} bytes; maximum is {maximum} bytes")]
    StreamNameTooLong { size: usize, maximum: usize },
    #[error("RTMP stream query is {size} bytes; maximum is {maximum} bytes")]
    QueryTooLong { size: usize, maximum: usize },
}

fn validated_rtmp_path<'a>(
    application: &'a str,
    protocol_name: &'a str,
) -> Result<(&'a str, &'a str, Option<&'a str>), RtmpStreamPathError> {
    validate_application(application)?;
    let (stream_name, query) = match protocol_name.split_once('?') {
        Some((_, "")) => return Err(RtmpStreamPathError::Query),
        Some((stream_name, query)) => (stream_name, Some(query)),
        None => (protocol_name, None),
    };
    if stream_name.len() > MAX_RTMP_STREAM_NAME_BYTES {
        return Err(RtmpStreamPathError::StreamNameTooLong {
            size: stream_name.len(),
            maximum: MAX_RTMP_STREAM_NAME_BYTES,
        });
    }
    validate_path_component(stream_name).map_err(|()| RtmpStreamPathError::StreamName)?;
    if let Some(query) = query {
        if query.len() > MAX_RTMP_QUERY_BYTES {
            return Err(RtmpStreamPathError::QueryTooLong {
                size: query.len(),
                maximum: MAX_RTMP_QUERY_BYTES,
            });
        }
        if query.contains('#') || query.chars().any(char::is_control) {
            return Err(RtmpStreamPathError::Query);
        }
    }
    Ok((application, stream_name, query))
}

fn validate_application(application: &str) -> Result<(), RtmpStreamPathError> {
    if application.len() > MAX_RTMP_APPLICATION_BYTES {
        return Err(RtmpStreamPathError::ApplicationTooLong {
            size: application.len(),
            maximum: MAX_RTMP_APPLICATION_BYTES,
        });
    }
    validate_path_component(application).map_err(|()| RtmpStreamPathError::Application)?;
    if application.contains('?') {
        return Err(RtmpStreamPathError::Application);
    }
    Ok(())
}

fn validate_path_component(component: &str) -> Result<(), ()> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['/', '#'])
        || component.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(())
}

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

struct MutableRecorder {
    id: RecorderId,
    name: Option<String>,
    manual: bool,
    phase: RecorderPhase,
    changed_at_unix_ms: u64,
    bytes_written: u64,
    current_relative_name: Option<String>,
    last_completed_relative_name: Option<String>,
    recoverable_partial_name: Option<String>,
    published_but_not_durable_relative_name: Option<String>,
    queue_messages: usize,
    queue_bytes: usize,
    events_enqueued: u64,
    events_processed: u64,
    events_dropped: u64,
    segments_started: u64,
    segments_completed: u64,
    discontinuities: u64,
    last_notification: Option<RecorderNotification>,
    worker_generation: u64,
    control: Option<Arc<RecorderController>>,
}

struct MutableRelay {
    id: RelayId,
    control: Arc<RtmpRelayController>,
}

impl MutableRelay {
    fn snapshot(&self) -> RelaySnapshot {
        RelaySnapshot {
            id: self.id,
            status: self.control.status(),
        }
    }
}

trait IntoRecorderControl {
    fn into_recorder_control(self) -> Option<Arc<RecorderController>>;
}

impl IntoRecorderControl for Option<Arc<RecorderController>> {
    fn into_recorder_control(self) -> Option<Arc<RecorderController>> {
        self
    }
}

impl IntoRecorderControl for Arc<RecorderController> {
    fn into_recorder_control(self) -> Option<Arc<RecorderController>> {
        Some(self)
    }
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
            current_relative_name: self.current_relative_name.clone(),
            last_completed_relative_name: self.last_completed_relative_name.clone(),
            recoverable_partial_name: self.recoverable_partial_name.clone(),
            published_but_not_durable_relative_name: self
                .published_but_not_durable_relative_name
                .clone(),
            queue_messages: self.queue_messages,
            queue_bytes: self.queue_bytes,
            events_enqueued: self.events_enqueued,
            events_processed: self.events_processed,
            events_dropped: self.events_dropped,
            segments_started: self.segments_started,
            segments_completed: self.segments_completed,
            discontinuities: self.discontinuities,
            last_notification: self.last_notification,
        }
    }
}

struct MutableStream {
    id: StreamId,
    revision: u64,
    key: StreamKey,
    created_at_unix_ms: u64,
    publisher: Option<PublisherSnapshot>,
    publisher_activity_at_unix_ms: u64,
    subscribers: HashMap<SessionId, usize>,
    media: MediaSnapshot,
    media_sample_sequence: u64,
    relays: Vec<MutableRelay>,
    recorders: HashMap<RecorderId, MutableRecorder>,
    recorder_order: Vec<RecorderId>,
}

impl MutableStream {
    fn new(key: StreamKey, at_unix_ms: u64) -> Self {
        Self {
            id: StreamId::new(),
            revision: 0,
            key,
            created_at_unix_ms: at_unix_ms,
            publisher: None,
            publisher_activity_at_unix_ms: 0,
            subscribers: HashMap::new(),
            media: MediaSnapshot::default(),
            media_sample_sequence: 0,
            relays: Vec::new(),
            recorders: HashMap::new(),
            recorder_order: Vec::new(),
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
            subscriber_count: self.subscribers.values().sum(),
            media: self.media,
            relays: self.relays.iter().map(MutableRelay::snapshot).collect(),
            recorders,
        }
    }
}

struct RegistryInner {
    admission_open: bool,
    revision: u64,
    as_of_unix_ms: u64,
    streams: HashMap<StreamId, MutableStream>,
    streams_by_key: HashMap<StreamKey, StreamId>,
    current: Arc<RtmpCatalogSnapshot>,
    snapshot_dirty: bool,
    work_stats: RtmpRegistryWorkStats,
}

pub struct RtmpRegistry {
    capabilities: RtmpCapabilities,
    inner: Mutex<RegistryInner>,
    sessions: Mutex<HashMap<SessionId, Arc<RtmpSessionControl>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PublisherOwner {
    stream_id: StreamId,
    session_id: SessionId,
}

impl RtmpRegistry {
    #[must_use]
    pub fn new(capabilities: RtmpCapabilities) -> Self {
        Self {
            capabilities,
            inner: Mutex::new(RegistryInner {
                admission_open: true,
                revision: 0,
                as_of_unix_ms: 0,
                streams: HashMap::new(),
                streams_by_key: HashMap::new(),
                current: Arc::new(RtmpCatalogSnapshot {
                    revision: 0,
                    as_of_unix_ms: 0,
                    capabilities,
                    streams: Vec::new(),
                }),
                snapshot_dirty: false,
                work_stats: RtmpRegistryWorkStats::default(),
            }),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Arc<RtmpCatalogSnapshot> {
        let mut inner = self.lock();
        refresh_recorder_statuses(&mut inner);
        if inner
            .streams
            .values()
            .any(|stream| !stream.relays.is_empty())
        {
            inner.snapshot_dirty = true;
        }
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
        Arc::clone(&inner.current)
    }

    #[must_use]
    pub fn work_stats(&self) -> RtmpRegistryWorkStats {
        self.lock().work_stats
    }

    /// Returns bounded snapshots of currently registered RTMP client sessions.
    #[must_use]
    pub fn session_snapshots(&self) -> Vec<RtmpClientSnapshot> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut snapshots: Vec<_> = sessions
            .values()
            .map(|session| session.snapshot())
            .collect();
        snapshots.sort_by_key(|session| session.session_id);
        snapshots
    }

    /// Queues a target-checked disconnect for one live RTMP session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session disappeared, its revision changed, its role changed, or
    /// another incompatible control request is pending.
    pub fn request_session_control(
        &self,
        session_id: SessionId,
        action: RtmpSessionControlAction,
        expected_revision: u64,
    ) -> Result<RtmpSessionControlOutcome, RtmpSessionControlError> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .get(&session_id)
            .ok_or(RtmpSessionControlError::NotFound)?
            .request(action, expected_revision)
    }

    pub(crate) fn register_session(
        self: &Arc<Self>,
        session_id: SessionId,
        service_id: &str,
        peer_addr: Option<std::net::IpAddr>,
    ) -> Option<Arc<RtmpSessionControl>> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions.len() >= MAX_RTMP_SESSION_CONTROLS {
            return None;
        }
        let control = RtmpSessionControl::new(session_id, service_id, peer_addr);
        sessions.insert(session_id, Arc::clone(&control));
        Some(control)
    }

    pub(crate) fn unregister_session(&self, session_id: SessionId) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    pub(crate) fn create_recorder_reaper(
        self: &Arc<Self>,
        capacity: usize,
    ) -> (Arc<RecorderReaperOwner>, RecorderReaperHandle) {
        RecorderReaper::start(capacity, Arc::downgrade(self))
    }

    pub(crate) fn close_admission(&self) {
        self.lock().admission_open = false;
    }

    /// Registers a publisher whose catalog entry is removed when the returned owner is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when another publisher already owns the stream.
    pub fn register_publisher(
        self: &Arc<Self>,
        key: StreamKey,
        session_id: SessionId,
        recorder_definitions: Vec<RecorderDefinition>,
        at_unix_ms: u64,
    ) -> Result<PublisherRegistration, CatalogError> {
        let stream_id = self.attach_publisher(key, session_id, recorder_definitions, at_unix_ms)?;
        Ok(PublisherRegistration {
            registry: Arc::clone(self),
            stream_id,
            session_id,
            recorder_ids: self.recorder_ids(stream_id, session_id)?,
            last_observed_at_unix_ms: at_unix_ms,
            active: true,
        })
    }

    pub(crate) fn register_managed_publisher(
        self: &Arc<Self>,
        key: StreamKey,
        session_id: SessionId,
        recorders: Vec<(RecorderDefinition, Arc<RecorderController>)>,
        relays: Vec<Arc<RtmpRelayController>>,
        at_unix_ms: u64,
    ) -> Result<PublisherRegistration, CatalogError> {
        let stream_id =
            self.attach_publisher_inner(key, session_id, recorders, relays, at_unix_ms)?;
        Ok(PublisherRegistration {
            registry: Arc::clone(self),
            stream_id,
            session_id,
            recorder_ids: self.recorder_ids(stream_id, session_id)?,
            last_observed_at_unix_ms: at_unix_ms,
            active: true,
        })
    }

    pub(crate) fn stale_publisher_owner(
        &self,
        key: &StreamKey,
        now_unix_ms: u64,
        threshold_unix_ms: u64,
    ) -> Option<PublisherOwner> {
        let inner = self.lock();
        let stream_id = inner.streams_by_key.get(key).copied()?;
        let stream = inner.streams.get(&stream_id)?;
        let publisher = stream.publisher?;
        if now_unix_ms.saturating_sub(stream.publisher_activity_at_unix_ms) < threshold_unix_ms {
            return None;
        }
        Some(PublisherOwner {
            stream_id,
            session_id: publisher.session_id,
        })
    }

    pub(crate) fn detach_expected_publisher(
        &self,
        owner: PublisherOwner,
        at_unix_ms: u64,
    ) -> Result<PublisherShutdown, CatalogError> {
        let mut inner = self.lock();
        detach_publisher_inner(
            &mut inner,
            owner.stream_id,
            owner.session_id,
            at_unix_ms,
            self.capabilities,
        )
    }

    /// Registers a subscriber whose catalog entry is removed when the returned owner is dropped.
    ///
    /// # Errors
    ///
    /// This operation currently has no expected error state.
    pub fn register_subscriber(
        self: &Arc<Self>,
        key: StreamKey,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<SubscriberRegistration, CatalogError> {
        let stream_id = self.attach_subscriber(key, session_id, at_unix_ms)?;
        Ok(SubscriberRegistration {
            registry: Arc::clone(self),
            stream_id,
            session_id,
            last_observed_at_unix_ms: at_unix_ms,
            active: true,
        })
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
        if !recorder_definitions.is_empty() {
            return Err(CatalogError::RecordingUnavailable);
        }
        self.attach_publisher_inner(
            key,
            session_id,
            recorder_definitions
                .into_iter()
                .map(|definition| (definition, None))
                .collect(),
            Vec::new(),
            at_unix_ms,
        )
    }

    fn attach_publisher_inner<C>(
        &self,
        key: StreamKey,
        session_id: SessionId,
        recorder_definitions: Vec<(RecorderDefinition, C)>,
        relays: Vec<Arc<RtmpRelayController>>,
        at_unix_ms: u64,
    ) -> Result<StreamId, CatalogError>
    where
        C: IntoRecorderControl,
    {
        let mut inner = self.lock();
        if !inner.admission_open {
            return Err(CatalogError::AdmissionClosed);
        }
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
        stream.publisher_activity_at_unix_ms = at_unix_ms;
        stream.media = MediaSnapshot::default();
        stream.media_sample_sequence = 0;
        stream.relays = relays
            .into_iter()
            .map(|control| MutableRelay {
                id: RelayId::new(),
                control,
            })
            .collect();
        let mut recorder_order = Vec::with_capacity(recorder_definitions.len());
        stream.recorders = recorder_definitions
            .into_iter()
            .map(|(definition, control)| {
                let id = RecorderId::new();
                recorder_order.push(id);
                (
                    id,
                    MutableRecorder {
                        id,
                        name: definition.name,
                        manual: definition.manual,
                        phase: RecorderPhase::Idle,
                        changed_at_unix_ms: at_unix_ms,
                        bytes_written: 0,
                        current_relative_name: None,
                        last_completed_relative_name: None,
                        recoverable_partial_name: None,
                        published_but_not_durable_relative_name: None,
                        queue_messages: 0,
                        queue_bytes: 0,
                        events_enqueued: 0,
                        events_processed: 0,
                        events_dropped: 0,
                        segments_started: 0,
                        segments_completed: 0,
                        discontinuities: 0,
                        last_notification: None,
                        worker_generation: 0,
                        control: control.into_recorder_control(),
                    },
                )
            })
            .collect();
        stream.recorder_order = recorder_order;
        stream.revision = stream.revision.saturating_add(1);
        mark_mutation(&mut inner, at_unix_ms);
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
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
        stream.publisher_activity_at_unix_ms = stream.publisher_activity_at_unix_ms.max(at_unix_ms);
        if sequence <= stream.media_sample_sequence {
            return Ok(false);
        }

        stream.media_sample_sequence = sequence;
        stream.media = media;
        stream.revision = stream.revision.saturating_add(1);
        inner.work_stats.media_updates = inner.work_stats.media_updates.saturating_add(1);
        mark_mutation(&mut inner, at_unix_ms);
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
        if !inner.admission_open {
            return Err(CatalogError::AdmissionClosed);
        }
        let stream_id = ensure_stream(&mut inner, key, at_unix_ms);
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        let count = stream.subscribers.entry(session_id).or_default();
        *count = count.saturating_add(1);
        stream.revision = stream.revision.saturating_add(1);
        mark_mutation(&mut inner, at_unix_ms);
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
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
        self.detach_publisher_deferred(stream_id, session_id, at_unix_ms)?
            .shutdown(at_unix_ms);
        Ok(())
    }

    fn detach_publisher_deferred(
        &self,
        stream_id: StreamId,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<PublisherShutdown, CatalogError> {
        let mut inner = self.lock();
        detach_publisher_inner(
            &mut inner,
            stream_id,
            session_id,
            at_unix_ms,
            self.capabilities,
        )
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
            let remove_session = match stream.subscribers.get_mut(&session_id) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => true,
                None => {
                    return Err(CatalogError::SubscriberNotFound {
                        stream_id,
                        session_id,
                    });
                }
            };
            if remove_session {
                stream.subscribers.remove(&session_id);
            }
            stream.revision = stream.revision.saturating_add(1);
            stream.publisher.is_none() && stream.subscribers.is_empty()
        };
        if remove {
            remove_stream(&mut inner, stream_id);
        }
        mark_mutation(&mut inner, at_unix_ms);
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
        Ok(())
    }

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

    fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner.lock().expect("RTMP registry mutex poisoned")
    }

    fn recorder_ids(
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

    fn recorder_snapshot(
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

fn detach_publisher_inner(
    inner: &mut RegistryInner,
    stream_id: StreamId,
    session_id: SessionId,
    at_unix_ms: u64,
    capabilities: RtmpCapabilities,
) -> Result<PublisherShutdown, CatalogError> {
    let (remove, recorder_controls, relay_controls) = {
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
        stream.publisher_activity_at_unix_ms = 0;
        stream.media = MediaSnapshot::default();
        stream.media_sample_sequence = 0;
        let recorder_controls = stream
            .recorders
            .values()
            .filter_map(|recorder| recorder.control.clone())
            .collect::<Vec<_>>();
        let relay_controls = stream
            .relays
            .drain(..)
            .map(|relay| relay.control)
            .collect::<Vec<_>>();
        stream.recorders.clear();
        stream.recorder_order.clear();
        stream.revision = stream.revision.saturating_add(1);
        (
            stream.subscribers.is_empty(),
            recorder_controls,
            relay_controls,
        )
    };
    if remove {
        remove_stream(inner, stream_id);
    }
    mark_mutation(inner, at_unix_ms);
    publish_snapshot_if_dirty(inner, capabilities);
    Ok(PublisherShutdown {
        recorder_controls,
        relay_controls,
    })
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

fn refresh_recorder_statuses(inner: &mut RegistryInner) {
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

fn apply_runtime_status(recorder: &mut MutableRecorder, runtime: RecorderRuntimeStatus) -> bool {
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

/// RAII ownership of one publisher entry in an [`RtmpRegistry`].
pub struct PublisherRegistration {
    registry: Arc<RtmpRegistry>,
    stream_id: StreamId,
    session_id: SessionId,
    recorder_ids: Vec<RecorderId>,
    last_observed_at_unix_ms: u64,
    active: bool,
}

pub(crate) struct PublisherShutdown {
    recorder_controls: Vec<Arc<RecorderController>>,
    relay_controls: Vec<Arc<RtmpRelayController>>,
}

impl PublisherShutdown {
    fn empty() -> Self {
        Self {
            recorder_controls: Vec::new(),
            relay_controls: Vec::new(),
        }
    }

    pub(crate) fn shutdown(self, at_unix_ms: u64) {
        for control in self.recorder_controls {
            control.deactivate(at_unix_ms);
        }
        for control in self.relay_controls {
            control.deactivate();
        }
    }
}

impl PublisherRegistration {
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    #[must_use]
    pub fn recorder_ids(&self) -> &[RecorderId] {
        &self.recorder_ids
    }

    pub fn observe_at(&mut self, at_unix_ms: u64) {
        self.last_observed_at_unix_ms = self.last_observed_at_unix_ms.max(at_unix_ms);
    }

    /// Explicitly releases the registration at the supplied catalog timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if this registration is no longer the catalog publisher.
    pub fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        self.release_deferred(at_unix_ms)?.shutdown(at_unix_ms);
        Ok(())
    }

    pub(crate) fn release_deferred(
        &mut self,
        at_unix_ms: u64,
    ) -> Result<PublisherShutdown, CatalogError> {
        if !self.active {
            return Ok(PublisherShutdown::empty());
        }
        let at_unix_ms = at_unix_ms.max(self.last_observed_at_unix_ms);
        let shutdown =
            self.registry
                .detach_publisher_deferred(self.stream_id, self.session_id, at_unix_ms)?;
        self.active = false;
        Ok(shutdown)
    }

    pub(crate) const fn last_observed_at_unix_ms(&self) -> u64 {
        self.last_observed_at_unix_ms
    }
}

impl Drop for PublisherRegistration {
    fn drop(&mut self) {
        if self.active {
            let _ = self.registry.detach_publisher(
                self.stream_id,
                self.session_id,
                self.last_observed_at_unix_ms,
            );
        }
    }
}

/// RAII ownership of one subscriber entry in an [`RtmpRegistry`].
pub struct SubscriberRegistration {
    registry: Arc<RtmpRegistry>,
    stream_id: StreamId,
    session_id: SessionId,
    last_observed_at_unix_ms: u64,
    active: bool,
}

impl SubscriberRegistration {
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn observe_at(&mut self, at_unix_ms: u64) {
        self.last_observed_at_unix_ms = self.last_observed_at_unix_ms.max(at_unix_ms);
    }

    /// Explicitly releases the registration at the supplied catalog timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if this registration is no longer present in the catalog.
    pub fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        if !self.active {
            return Ok(());
        }
        self.registry
            .detach_subscriber(self.stream_id, self.session_id, at_unix_ms)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for SubscriberRegistration {
    fn drop(&mut self) {
        if self.active {
            let _ = self.registry.detach_subscriber(
                self.stream_id,
                self.session_id,
                self.last_observed_at_unix_ms,
            );
        }
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

fn mark_mutation(inner: &mut RegistryInner, at_unix_ms: u64) {
    inner.revision = inner.revision.saturating_add(1);
    inner.as_of_unix_ms = inner.as_of_unix_ms.max(at_unix_ms);
    inner.snapshot_dirty = true;
}

fn publish_snapshot_if_dirty(inner: &mut RegistryInner, capabilities: RtmpCapabilities) {
    if !inner.snapshot_dirty {
        return;
    }
    let mut streams: Vec<_> = inner
        .streams
        .values()
        .map(MutableStream::snapshot)
        .collect();
    streams.sort_by(|left, right| left.key.cmp(&right.key).then(left.id.cmp(&right.id)));
    inner.work_stats.snapshot_rebuilds = inner.work_stats.snapshot_rebuilds.saturating_add(1);
    inner.work_stats.snapshot_streams_visited = inner
        .work_stats
        .snapshot_streams_visited
        .saturating_add(streams.len() as u64);
    inner.current = Arc::new(RtmpCatalogSnapshot {
        revision: inner.revision,
        as_of_unix_ms: inner.as_of_unix_ms,
        capabilities,
        streams,
    });
    inner.snapshot_dirty = false;
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        sync::{Arc, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use rustix::fs::{FlockOperation, flock};
    use tempfile::tempdir;

    use super::*;
    use crate::{
        RecorderFailure, RecorderWorkerConfig, RecordingPathPolicy, RecordingStore,
        RecordingStoreLimits, RtmpRecorderPolicy, RtmpRecorderStart,
    };

    #[test]
    fn publisher_activity_is_monotonic_across_metadata_and_wall_clock_rollback() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        }));
        let key = StreamKey::new("edge", "live", "camera");
        let subscriber = SessionId::new();
        let stream_id = registry
            .attach_subscriber(key.clone(), subscriber, 1)
            .expect("subscriber");
        let publisher = SessionId::new();
        registry
            .attach_publisher(key.clone(), publisher, Vec::new(), 10_000)
            .expect("publisher");
        registry
            .update_media_sample(stream_id, publisher, 1, MediaSnapshot::default(), 20_000)
            .expect("metadata activity sample");
        registry
            .update_media_sample(stream_id, publisher, 2, MediaSnapshot::default(), 15_000)
            .expect("wall-clock rollback sample");

        assert!(
            registry
                .stale_publisher_owner(
                    &key,
                    20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS - 1,
                    crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS,
                )
                .is_none()
        );
        let owner = registry
            .stale_publisher_owner(
                &key,
                20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS,
                crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS,
            )
            .expect("stale publisher");
        let shutdown = registry
            .detach_expected_publisher(owner, 20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS)
            .expect("stale publisher detach");
        shutdown.shutdown(20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS);

        let snapshot = registry.snapshot();
        let stream = snapshot
            .streams
            .iter()
            .find(|stream| stream.id == stream_id)
            .expect("subscriber keeps stream");
        assert!(stream.publisher.is_none());
        assert_eq!(stream.media, MediaSnapshot::default());
        assert!(stream.recorders.is_empty());
    }

    #[test]
    fn terminal_admission_rejects_new_recorder_start() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let session_id = SessionId::new();
        let stream_id = registry
            .attach_publisher_inner(
                StreamKey::new("live", "broadcast", "camera"),
                session_id,
                vec![(
                    RecorderDefinition::manual(Some("archive".into())),
                    None::<Arc<RecorderController>>,
                )],
                Vec::new(),
                1,
            )
            .expect("publisher");
        let recorder_id = registry
            .recorder_ids(stream_id, session_id)
            .expect("recorder IDs")[0];

        registry.close_admission();

        assert!(matches!(
            registry.start_recording(stream_id, recorder_id, 2),
            Err(CatalogError::AdmissionClosed)
        ));
    }

    #[test]
    fn reaper_backpressures_generations_while_registry_completion_is_blocked() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders: 4,
            },
        )
        .expect("recording store");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
        let completing =
            test_controller(&store, &reaper, "completing", 1024, Duration::from_secs(1));
        let first_pending = test_controller(
            &store,
            &reaper,
            "first-pending",
            1024,
            Duration::from_secs(1),
        );
        let second_pending = test_controller(
            &store,
            &reaper,
            "second-pending",
            1024,
            Duration::from_secs(1),
        );
        let completing_context = recorder_context();
        completing
            .start(completing_context)
            .expect("start completing generation");
        first_pending
            .start(recorder_context())
            .expect("start first pending generation");
        second_pending
            .start(recorder_context())
            .expect("start second pending generation");

        let registry_lock = registry.lock();
        assert!(completing.stop(completing_context));
        wait_until(Duration::from_secs(2), || !completing.status().stopping);

        let pending = [Arc::clone(&first_pending), Arc::clone(&second_pending)];
        let (completed_tx, completed_rx) = mpsc::channel();
        let submitter = thread::spawn(move || {
            for controller in pending {
                controller.deactivate(2_000);
            }
            completed_tx.send(()).expect("report submitted generations");
        });
        assert!(
            matches!(
                completed_rx.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "submission must backpressure while the prior generation awaits registry completion"
        );

        drop(registry_lock);
        completed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("pending generations submitted after completion");
        submitter.join().expect("generation submitter");
        wait_until(Duration::from_secs(2), || {
            !first_pending.status().stopping && !second_pending.status().stopping
        });
        drop(reaper_owner);
    }

    #[test]
    fn restart_releases_controller_lock_while_stale_worker_awaits_reaper_capacity() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders: 4,
            },
        )
        .expect("recording store");
        let ownership = OpenOptions::new()
            .read(true)
            .open(root.path())
            .expect("recording root ownership");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
        let controller = test_controller(&store, &reaper, "camera", 8, Duration::from_secs(5));

        let first_context = recorder_context();
        controller
            .start(first_context)
            .expect("start first generation");
        flock(&ownership, FlockOperation::LockExclusive).expect("stall storage admission");
        assert_eq!(
            controller.try_enqueue(
                crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
                1_100,
            ),
            RecorderEnqueueResult::Queued
        );
        thread::sleep(Duration::from_millis(20));
        assert!(controller.stop(first_context));

        controller
            .start(recorder_context())
            .expect("start second generation");
        assert_eq!(
            controller.try_enqueue(
                crate::MediaEvent::audio(0, Arc::<[u8]>::from(vec![0; 32]))
                    .expect("oversized audio event"),
                1_200,
            ),
            RecorderEnqueueResult::DroppedDiscontinuity
        );
        wait_until(Duration::from_secs(2), || {
            controller
                .status()
                .status
                .is_some_and(|status| matches!(status.phase, RecorderWorkerPhase::Failed(_)))
        });

        let restart = Arc::clone(&controller);
        let restart_context = recorder_context();
        let (started_tx, started_rx) = mpsc::channel();
        let restart_thread = thread::spawn(move || {
            started_tx
                .send(restart.start(restart_context))
                .expect("report restart result");
        });
        assert!(
            matches!(
                started_rx.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "restart must backpressure behind the first generation"
        );

        let observed = Arc::clone(&controller);
        let (observed_tx, observed_rx) = mpsc::channel();
        let observer_thread = thread::spawn(move || {
            observed.status();
            let _ = observed_tx.send(());
        });
        observed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("restart must not retain the controller lock while backpressured");
        observer_thread.join().expect("controller observer");

        flock(&ownership, FlockOperation::Unlock).expect("release storage admission");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("restart completes after reaper capacity is released")
            .expect("restart third generation");
        restart_thread.join().expect("restart thread");
        controller.deactivate(1_300);
        wait_until(Duration::from_secs(2), || !controller.status().stopping);
        drop(reaper_owner);
    }

    #[test]
    fn proactive_shutdown_transitions_recording_through_stopping_to_idle() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let ownership = OpenOptions::new()
            .read(true)
            .open(root.path())
            .expect("recording root ownership");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
        let controller = test_controller(&store, &reaper, "camera", 1024, Duration::from_secs(1));
        let session_id = SessionId::new();
        let registration = registry
            .register_managed_publisher(
                StreamKey::new("edge", "live", "camera"),
                session_id,
                vec![(
                    RecorderDefinition::automatic(Some("archive".to_owned())),
                    Arc::clone(&controller),
                )],
                Vec::new(),
                1_000,
            )
            .expect("managed publisher");
        let stream_id = registration.stream_id();
        let recorder_id = registration.recorder_ids()[0];
        registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);
        assert!(matches!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("recording snapshot")
                .phase,
            RecorderPhase::Recording { .. }
        ));
        flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
        assert_eq!(
            controller.try_enqueue(
                crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
                1_100,
            ),
            RecorderEnqueueResult::Queued
        );
        thread::sleep(Duration::from_millis(20));

        let shutdown = reaper_owner.initiate_shutdown(Instant::now() + Duration::from_secs(1));
        registry.initiate_recorder_shutdown(&reaper);

        assert!(matches!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("stopping snapshot")
                .phase,
            RecorderPhase::Stopping { .. }
        ));
        flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
        assert!(shutdown.wait_until(Instant::now() + Duration::from_secs(1)));
        wait_until(Duration::from_secs(1), || {
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .is_ok_and(|snapshot| snapshot.phase == RecorderPhase::Idle)
        });
        drop(registration);
    }

    #[test]
    fn continuous_recorder_returns_from_failed_to_recording_in_one_publisher() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
        let controller = test_controller_with_start(
            &store,
            &reaper,
            "camera",
            1024,
            Duration::from_secs(1),
            RtmpRecorderStart::Continuous,
        );
        let session_id = SessionId::new();
        let registration = registry
            .register_managed_publisher(
                StreamKey::new("edge", "live", "camera"),
                session_id,
                vec![(
                    RecorderDefinition::automatic(Some("archive".to_owned())),
                    Arc::clone(&controller),
                )],
                Vec::new(),
                1_000,
            )
            .expect("managed publisher");
        let stream_id = registration.stream_id();
        let recorder_id = registration.recorder_ids()[0];
        registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);

        assert_continuous_catalog_recovery(
            &registry,
            &controller,
            stream_id,
            session_id,
            recorder_id,
        );
        drop(registration);
        drop(reaper_owner);
    }

    #[test]
    fn continuous_start_failure_counts_media_dropped_before_a_worker_exists() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let lease = store.acquire_recorder().expect("occupy recorder capacity");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
        let controller = test_controller_with_start(
            &store,
            &reaper,
            "camera",
            1024,
            Duration::from_secs(1),
            RtmpRecorderStart::Continuous,
        );
        let session_id = SessionId::new();
        let registration = registry
            .register_managed_publisher(
                StreamKey::new("edge", "live", "camera"),
                session_id,
                vec![(
                    RecorderDefinition::automatic(Some("archive".to_owned())),
                    Arc::clone(&controller),
                )],
                Vec::new(),
                1_000,
            )
            .expect("managed publisher");
        let stream_id = registration.stream_id();
        let recorder_id = registration.recorder_ids()[0];
        registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);

        let event = crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"dropped"[..]))
            .expect("recovery event");
        let enqueue_result = controller.try_enqueue(event, 1_100);
        assert_eq!(enqueue_result, RecorderEnqueueResult::Inactive);
        assert!(controller.status().status.is_none());
        registry.update_recorder_runtime(stream_id, session_id, recorder_id, enqueue_result, 1_100);

        assert_eq!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("failed recorder snapshot")
                .events_dropped,
            1
        );
        drop(lease);
        drop(registration);
        drop(reaper_owner);
    }

    #[test]
    fn proactive_shutdown_transitions_stalled_recording_from_stopping_to_failed() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024),
                max_files: Some(2),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let ownership = OpenOptions::new()
            .read(true)
            .open(root.path())
            .expect("recording root ownership");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
        let controller = test_controller(&store, &reaper, "camera", 1024, Duration::from_secs(1));
        let session_id = SessionId::new();
        let registration = registry
            .register_managed_publisher(
                StreamKey::new("edge", "live", "camera"),
                session_id,
                vec![(
                    RecorderDefinition::automatic(Some("archive".to_owned())),
                    Arc::clone(&controller),
                )],
                Vec::new(),
                1_000,
            )
            .expect("managed publisher");
        let stream_id = registration.stream_id();
        let recorder_id = registration.recorder_ids()[0];
        registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);
        flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
        assert_eq!(
            controller.try_enqueue(
                crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
                1_100,
            ),
            RecorderEnqueueResult::Queued
        );
        thread::sleep(Duration::from_millis(20));

        let shutdown_deadline = Instant::now() + Duration::from_millis(50);
        let shutdown = reaper_owner.initiate_shutdown(shutdown_deadline);
        registry.initiate_recorder_shutdown(&reaper);
        assert!(matches!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("stopping snapshot")
                .phase,
            RecorderPhase::Stopping { .. }
        ));
        assert!(shutdown.wait_until(Instant::now() + Duration::from_secs(1)));
        assert!(matches!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("failed snapshot")
                .phase,
            RecorderPhase::Failed {
                code: RecorderErrorCode::ShutdownTimedOut,
                ..
            }
        ));
        flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
        drop(registration);
    }

    #[test]
    fn catalog_start_after_shutdown_snapshot_fails_terminally() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024),
                max_files: Some(2),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
        let shutdown = reaper_owner.initiate_shutdown(Instant::now() + Duration::from_secs(1));
        let controller = test_controller(&store, &reaper, "camera", 1024, Duration::from_secs(1));
        let session_id = SessionId::new();
        let registration = registry
            .register_managed_publisher(
                StreamKey::new("edge", "live", "camera"),
                session_id,
                vec![(
                    RecorderDefinition::manual(Some("archive".to_owned())),
                    controller,
                )],
                Vec::new(),
                1_000,
            )
            .expect("managed publisher");
        let stream_id = registration.stream_id();
        let recorder_id = registration.recorder_ids()[0];

        assert!(matches!(
            registry.start_recording(stream_id, recorder_id, 1_100),
            Err(CatalogError::RecorderFailed {
                code: RecorderErrorCode::BackendUnavailable,
                ..
            })
        ));
        assert!(matches!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("failed recorder snapshot")
                .phase,
            RecorderPhase::Failed {
                code: RecorderErrorCode::BackendUnavailable,
                ..
            }
        ));
        assert_eq!(store.stats().active_recorders, 0);
        assert!(shutdown.wait_until(Instant::now() + Duration::from_secs(1)));
        drop(registration);
    }

    fn assert_continuous_catalog_recovery(
        registry: &Arc<RtmpRegistry>,
        controller: &Arc<RecorderController>,
        stream_id: StreamId,
        session_id: SessionId,
        recorder_id: RecorderId,
    ) {
        controller.fail_before_process();
        let failed_event =
            crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"failure"[..])).expect("failed event");
        assert_eq!(
            controller.try_enqueue(failed_event, 1_100),
            RecorderEnqueueResult::Queued
        );
        wait_until(Duration::from_secs(1), || {
            controller.status().status.is_some_and(|status| {
                status.phase == RecorderWorkerPhase::Failed(RecorderFailure::Open)
            })
        });
        registry.update_recorder_runtime(
            stream_id,
            session_id,
            recorder_id,
            RecorderEnqueueResult::Inactive,
            1_100,
        );
        assert!(matches!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("failed recorder snapshot")
                .phase,
            RecorderPhase::Failed { .. }
        ));
        assert_stale_recording_status_does_not_hide_failure(
            registry,
            controller,
            stream_id,
            recorder_id,
        );

        let recovery_event = crate::MediaEvent::audio(1, Arc::<[u8]>::from(&b"recover"[..]))
            .expect("recovery event");
        assert_eq!(
            controller.try_enqueue(recovery_event, 1_200),
            RecorderEnqueueResult::Inactive
        );
        wait_until(Duration::from_secs(1), || {
            let status = controller.status();
            !status.stopping && status.recovering
        });
        thread::sleep(Duration::from_millis(20));
        let continued_event = crate::MediaEvent::audio(2, Arc::<[u8]>::from(&b"continued"[..]))
            .expect("continued event");
        let enqueue_result = controller.try_enqueue(continued_event, 1_300);
        assert_eq!(enqueue_result, RecorderEnqueueResult::Queued);
        wait_until(Duration::from_secs(1), || {
            controller
                .status()
                .status
                .is_some_and(|status| status.phase == RecorderWorkerPhase::Recording)
        });
        registry.update_recorder_runtime(stream_id, session_id, recorder_id, enqueue_result, 1_300);
        assert!(matches!(
            registry
                .recorder_snapshot(stream_id, recorder_id)
                .expect("recovered recorder snapshot")
                .phase,
            RecorderPhase::Recording { .. }
        ));

        controller.deactivate(1_400);
        wait_until(Duration::from_secs(1), || !controller.status().stopping);
    }

    fn assert_stale_recording_status_does_not_hide_failure(
        registry: &RtmpRegistry,
        controller: &RecorderController,
        stream_id: StreamId,
        recorder_id: RecorderId,
    ) {
        let mut stale_runtime = controller.status();
        stale_runtime.stopping = false;
        stale_runtime.recovering = false;
        stale_runtime
            .status
            .as_mut()
            .expect("failed runtime status")
            .phase = RecorderWorkerPhase::Recording;
        let mut inner = registry.lock();
        let recorder = inner
            .streams
            .get_mut(&stream_id)
            .expect("managed stream")
            .recorders
            .get_mut(&recorder_id)
            .expect("managed recorder");
        apply_runtime_status(recorder, stale_runtime);
        assert!(matches!(recorder.phase, RecorderPhase::Failed { .. }));
    }

    fn test_controller(
        store: &RecordingStore,
        reaper: &RecorderReaperHandle,
        name: &'static str,
        max_queue_bytes: usize,
        shutdown_timeout: Duration,
    ) -> Arc<RecorderController> {
        test_controller_with_start(
            store,
            reaper,
            name,
            max_queue_bytes,
            shutdown_timeout,
            RtmpRecorderStart::Manual,
        )
    }

    fn test_controller_with_start(
        store: &RecordingStore,
        reaper: &RecorderReaperHandle,
        name: &'static str,
        max_queue_bytes: usize,
        shutdown_timeout: Duration,
        start: RtmpRecorderStart,
    ) -> Arc<RecorderController> {
        Arc::new(RecorderController::new(
            RtmpRecorderPolicy::new(
                "archive",
                start,
                store.clone(),
                RecordingPathPolicy::new(".flv", false).expect("recording path policy"),
                RecorderWorkerConfig {
                    max_queue_messages: 4,
                    max_queue_bytes,
                    rotation_interval: None,
                    shutdown_timeout,
                    video_codec: None,
                    ..RecorderWorkerConfig::default()
                },
            ),
            Arc::<[u8]>::from(name.as_bytes()),
            reaper.clone(),
            1_000,
        ))
    }

    fn recorder_context() -> RecorderCommandContext {
        RecorderCommandContext {
            stream_id: StreamId::new(),
            publisher_session_id: SessionId::new(),
            recorder_id: RecorderId::new(),
            operation_id: OperationId::new(),
            at_unix_ms: 1_000,
        }
    }

    fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + timeout;
        while !predicate() {
            assert!(Instant::now() < deadline, "condition timeout");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
