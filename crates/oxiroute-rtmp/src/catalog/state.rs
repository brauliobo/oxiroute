#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct MutableRecorder {
    pub(super) id: RecorderId,
    pub(super) name: Option<String>,
    pub(super) manual: bool,
    pub(super) phase: RecorderPhase,
    pub(super) changed_at_unix_ms: u64,
    pub(super) bytes_written: u64,
    pub(super) current_relative_name: Option<String>,
    pub(super) last_completed_relative_name: Option<String>,
    pub(super) recoverable_partial_name: Option<String>,
    pub(super) published_but_not_durable_relative_name: Option<String>,
    pub(super) queue_messages: usize,
    pub(super) queue_bytes: usize,
    pub(super) events_enqueued: u64,
    pub(super) events_processed: u64,
    pub(super) events_dropped: u64,
    pub(super) segments_started: u64,
    pub(super) segments_completed: u64,
    pub(super) discontinuities: u64,
    pub(super) last_notification: Option<RecorderNotification>,
    pub(super) worker_generation: u64,
    pub(super) control: Option<Arc<RecorderController>>,
}

pub(super) struct MutableRelay {
    pub(super) id: RelayId,
    pub(super) control: Arc<RtmpRelayController>,
}

impl MutableRelay {
    fn snapshot(&self) -> RelaySnapshot {
        RelaySnapshot {
            id: self.id,
            status: self.control.status(),
        }
    }
}

pub(super) trait IntoRecorderControl {
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
    pub(super) fn snapshot(&self) -> RecorderSnapshot {
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

pub(super) struct MutableStream {
    pub(super) id: StreamId,
    pub(super) revision: u64,
    pub(super) key: StreamKey,
    created_at_unix_ms: u64,
    pub(super) publisher: Option<PublisherSnapshot>,
    pub(super) publisher_activity_at_unix_ms: u64,
    pub(super) subscribers: HashMap<SessionId, usize>,
    pub(super) media: MediaSnapshot,
    pub(super) media_sample_sequence: u64,
    pub(super) relays: Vec<MutableRelay>,
    pub(super) recorders: HashMap<RecorderId, MutableRecorder>,
    pub(super) recorder_order: Vec<RecorderId>,
}

impl MutableStream {
    pub(super) fn new(key: StreamKey, at_unix_ms: u64) -> Self {
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

    pub(super) fn snapshot(&self) -> StreamSnapshot {
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

pub(super) struct RegistryInner {
    pub(super) admission_open: bool,
    pub(super) revision: u64,
    pub(super) as_of_unix_ms: u64,
    pub(super) streams: HashMap<StreamId, MutableStream>,
    pub(super) streams_by_key: HashMap<StreamKey, StreamId>,
    pub(super) current: Arc<RtmpCatalogSnapshot>,
    pub(super) snapshot_dirty: bool,
    pub(super) work_stats: RtmpRegistryWorkStats,
}

pub struct RtmpRegistry {
    pub(super) capabilities: RtmpCapabilities,
    pub(super) inner: Mutex<RegistryInner>,
    pub(super) sessions: Mutex<HashMap<SessionId, Arc<RtmpSessionControl>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PublisherOwner {
    pub(super) stream_id: StreamId,
    pub(super) session_id: SessionId,
}
