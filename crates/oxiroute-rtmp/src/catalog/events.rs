#[allow(clippy::wildcard_imports)]
use super::*;

pub const MAX_PENDING_CATALOG_EVENTS: usize = 512;

/// What a recorder did, as observed by the catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderTransitionKind {
    Started,
    Recovered,
    Stopped,
    Failed(RecorderErrorCode),
}

/// A recorder started, recovered, stopped, or failed on a stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderTransition {
    pub key: StreamKey,
    pub stream_id: StreamId,
    pub publisher_session_id: Option<SessionId>,
    pub recorder_id: RecorderId,
    pub recorder_name: Option<String>,
    pub kind: RecorderTransitionKind,
    pub at_unix_ms: u64,
}

/// A catalog change an operator wants in the event stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogEvent {
    PublisherEnded(EndedStreamSnapshot),
    Recorder(RecorderTransition),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecorderReport {
    Idle,
    Active,
    Failed(RecorderErrorCode),
}

impl RecorderReport {
    fn after(self, phase: RecorderPhase) -> Self {
        match phase {
            RecorderPhase::Idle => Self::Idle,
            RecorderPhase::Recording { .. } => Self::Active,
            RecorderPhase::Failed { code, .. } => Self::Failed(code),
            RecorderPhase::Starting { .. } | RecorderPhase::Stopping { .. } => self,
        }
    }

    fn transition(self, next: Self) -> Option<RecorderTransitionKind> {
        match (self, next) {
            (Self::Idle, Self::Active) => Some(RecorderTransitionKind::Started),
            (Self::Failed(_), Self::Active) => Some(RecorderTransitionKind::Recovered),
            (Self::Active, Self::Idle) => Some(RecorderTransitionKind::Stopped),
            (previous, Self::Failed(code)) if previous != Self::Failed(code) => {
                Some(RecorderTransitionKind::Failed(code))
            }
            _ => None,
        }
    }
}

impl RegistryInner {
    pub(super) fn push_event(&mut self, event: CatalogEvent) {
        if self.pending_events.len() == MAX_PENDING_CATALOG_EVENTS {
            self.pending_events.pop_front();
        }
        self.pending_events.push_back(event);
    }
}

pub(super) fn note_recorder_transitions(
    inner: &mut RegistryInner,
    stream_id: StreamId,
    at_unix_ms: u64,
) {
    let Some(stream) = inner.streams.get_mut(&stream_id) else {
        return;
    };
    let publisher_session_id = stream.publisher.map(|publisher| publisher.session_id);
    let mut events = Vec::new();
    for recorder in stream.recorders.values_mut() {
        let next = recorder.reported.after(recorder.phase);
        if let Some(kind) = recorder.reported.transition(next) {
            events.push(CatalogEvent::Recorder(RecorderTransition {
                key: stream.key.clone(),
                stream_id,
                publisher_session_id,
                recorder_id: recorder.id,
                recorder_name: recorder.name.clone(),
                kind,
                at_unix_ms,
            }));
        }
        recorder.reported = next;
    }
    for event in events {
        inner.push_event(event);
    }
}

pub(super) fn stopped_recorder_events(
    stream: &MutableStream,
    at_unix_ms: u64,
) -> Vec<CatalogEvent> {
    stream
        .recorders
        .values()
        .filter(|recorder| recorder.reported == RecorderReport::Active)
        .map(|recorder| {
            CatalogEvent::Recorder(RecorderTransition {
                key: stream.key.clone(),
                stream_id: stream.id,
                publisher_session_id: stream.publisher.map(|publisher| publisher.session_id),
                recorder_id: recorder.id,
                recorder_name: recorder.name.clone(),
                kind: RecorderTransitionKind::Stopped,
                at_unix_ms,
            })
        })
        .collect()
}

impl RtmpRegistry {
    /// Drains the catalog events that happened since the previous call, oldest first.
    #[must_use]
    pub fn take_events(&self) -> Vec<CatalogEvent> {
        self.lock().pending_events.drain(..).collect()
    }
}
