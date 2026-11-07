#[allow(clippy::wildcard_imports)]
use super::*;

/// Explicit service and per-viewer live fanout bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveHubLimits {
    pub max_streams: usize,
    pub max_subscribers: usize,
    pub max_subscribers_per_stream: usize,
    pub max_queue_messages_per_subscriber: usize,
    pub max_queue_bytes_per_subscriber: usize,
    pub max_fanout_bytes: usize,
    pub max_cached_metadata_bytes: usize,
    pub max_cached_codec_header_bytes: usize,
}

impl Default for LiveHubLimits {
    fn default() -> Self {
        Self {
            max_streams: 1_024,
            max_subscribers: 10_000,
            max_subscribers_per_stream: 1_000,
            max_queue_messages_per_subscriber: 256,
            max_queue_bytes_per_subscriber: 8 * 1_024 * 1_024,
            max_fanout_bytes: 256 * 1_024 * 1_024,
            max_cached_metadata_bytes: 1024 * 1024,
            max_cached_codec_header_bytes: 64 * 1024,
        }
    }
}

/// Current live fanout resource use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LiveHubStats {
    pub streams: usize,
    pub publishers: usize,
    pub subscribers: usize,
    pub fanout_bytes: usize,
}

/// Result of one nonblocking publication attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PublishReport {
    pub queued_events: usize,
    pub dropped_events: usize,
    pub viewers_resynchronized: usize,
    pub stream_fanout_bytes: usize,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum LiveHubError {
    #[error("live stream limit of {maximum} reached")]
    StreamLimitReached { maximum: usize },
    #[error("live subscriber service limit of {maximum} reached")]
    SubscriberLimitReached { maximum: usize },
    #[error("live subscriber limit of {maximum} reached for {key:?}")]
    StreamSubscriberLimitReached { key: StreamKey, maximum: usize },
    #[error("live stream {key:?} already has a publisher")]
    PublisherAlreadyAttached { key: StreamKey },
    #[error("publisher lease for live stream {key:?} is no longer active")]
    PublisherExpired { key: StreamKey },
    #[error("{kind:?} cache payload is {size} bytes; maximum is {maximum} bytes")]
    CachedEventTooLarge {
        kind: MediaEventKind,
        size: usize,
        maximum: usize,
    },
    #[error("live hub identity space exhausted")]
    IdentityExhausted,
}

/// Opaque identity for one publisher attachment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublisherIncarnation(u64);

impl PublisherIncarnation {
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Independent bounded live-stream fanout state.
#[derive(Clone)]
pub struct LiveHub {
    shared: Arc<LiveHubShared>,
}

pub(super) struct LiveHubShared {
    pub(super) limits: LiveHubLimits,
    roles: Mutex<()>,
    pub(super) state: Mutex<LiveHubState>,
}

#[derive(Default)]
pub(super) struct LiveHubState {
    pub(super) streams: BTreeMap<StreamKey, LiveStream>,
    pub(super) subscriber_count: usize,
    pub(super) fanout_bytes: usize,
    next_publisher_incarnation: u64,
    next_subscriber_id: u64,
}

#[derive(Default)]
pub(super) struct LiveStream {
    pub(super) publisher: Option<PublisherIncarnation>,
    pub(super) cache: MediaCache,
    pub(super) subscribers: BTreeMap<u64, SubscriberQueue>,
}

#[derive(Default)]
pub(super) struct MediaCache {
    metadata: Option<MediaEvent>,
    aac_sequence_header: Option<MediaEvent>,
    video_sequence_headers: BTreeMap<VideoCodec, MediaEvent>,
    video_track: CurrentVideoTrack,
}

impl MediaCache {
    pub(super) fn transition(&mut self, event: &MediaEvent) -> VideoTrackTransition {
        let next_track = match event.kind() {
            MediaEventKind::Metadata => Some(
                event
                    .stream_metadata()
                    .and_then(|metadata| metadata.video_codec_id)
                    .map_or(CurrentVideoTrack::Absent, |codec_id| {
                        if codec_id == u32::from(AVC_CODEC_ID) {
                            CurrentVideoTrack::Present(Some(VideoCodec::Avc))
                        } else {
                            CurrentVideoTrack::Present(None)
                        }
                    }),
            ),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {
                Some(CurrentVideoTrack::Present(event.video_codec()))
            }
            MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => None,
        };
        next_track.map_or(VideoTrackTransition::None, |next_track| {
            let previous = self.video_track;
            self.video_track = next_track;
            VideoTrackTransition::between(previous, next_track)
        })
    }

    pub(super) fn store(&mut self, event: MediaEvent) {
        match event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(event),
            MediaEventKind::AacSequenceHeader => self.aac_sequence_header = Some(event),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {
                let codec = event
                    .video_codec()
                    .expect("video sequence headers have a classified codec");
                self.video_sequence_headers.insert(codec, event);
            }
            MediaEventKind::Audio
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {}
        }
    }

    pub(super) fn mixed_bootstrap(&self, keyframe: &MediaEvent) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(4);
        events.extend(self.metadata.iter().cloned());
        events.extend(self.aac_sequence_header.iter().cloned());
        events.extend(self.video_sequence_header(keyframe).cloned());
        events.push(keyframe.clone());
        events
    }

    pub(super) fn audio_bootstrap(&self, audio: &MediaEvent) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(3);
        events.extend(self.metadata.iter().cloned());
        events.extend(self.aac_sequence_header.iter().cloned());
        events.push(audio.clone());
        events
    }

    pub(super) fn video_bootstrap(&self, keyframe: &MediaEvent) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(2);
        events.extend(self.video_sequence_header(keyframe).cloned());
        events.push(keyframe.clone());
        events
    }

    fn video_sequence_header(&self, keyframe: &MediaEvent) -> Option<&MediaEvent> {
        keyframe
            .video_codec()
            .and_then(|codec| self.video_sequence_headers.get(&codec))
    }

    const fn expects_video(&self) -> bool {
        matches!(self.video_track, CurrentVideoTrack::Present(_))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CurrentVideoTrack {
    #[default]
    Unknown,
    Absent,
    Present(Option<VideoCodec>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VideoTrackTransition {
    None,
    Absent,
    Present { codec_changed: bool },
}

impl VideoTrackTransition {
    fn between(previous: CurrentVideoTrack, next: CurrentVideoTrack) -> Self {
        if previous == next {
            return Self::None;
        }
        match next {
            CurrentVideoTrack::Unknown => Self::None,
            CurrentVideoTrack::Absent => Self::Absent,
            CurrentVideoTrack::Present(_) => Self::Present {
                codec_changed: matches!(previous, CurrentVideoTrack::Present(_)),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SubscriberState {
    AwaitingMedia,
    AwaitingKeyframe,
    Audio,
    Mixed,
}

pub(super) struct SubscriberQueue {
    pub(super) events: VecDeque<MediaEvent>,
    pub(super) bytes: usize,
    pub(super) state: SubscriberState,
}

impl SubscriberQueue {
    fn new(video_expected: bool) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            state: if video_expected {
                SubscriberState::AwaitingKeyframe
            } else {
                SubscriberState::AwaitingMedia
            },
        }
    }
}

impl LiveHub {
    #[must_use]
    pub fn new(limits: LiveHubLimits) -> Self {
        Self {
            shared: Arc::new(LiveHubShared {
                limits,
                roles: Mutex::new(()),
                state: Mutex::new(LiveHubState::default()),
            }),
        }
    }

    /// Attaches the sole publisher for an exact stream key.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream cap is reached, another publisher is attached, or an
    /// identity token cannot be allocated.
    pub fn attach_publisher(&self, key: StreamKey) -> Result<PublisherLease, LiveHubError> {
        let mut state = self.lock();
        if !state.streams.contains_key(&key)
            && state.streams.len() >= self.shared.limits.max_streams
        {
            return Err(LiveHubError::StreamLimitReached {
                maximum: self.shared.limits.max_streams,
            });
        }
        if state
            .streams
            .get(&key)
            .is_some_and(|stream| stream.publisher.is_some())
        {
            return Err(LiveHubError::PublisherAlreadyAttached { key });
        }

        self.create_publisher(&mut state, key)
    }

    /// Replaces the current publisher incarnation for an exact stream key.
    pub(crate) fn replace_publisher(&self, key: StreamKey) -> Result<PublisherLease, LiveHubError> {
        let mut state = self.lock();
        self.create_publisher(&mut state, key)
    }

    fn create_publisher(
        &self,
        state: &mut LiveHubState,
        key: StreamKey,
    ) -> Result<PublisherLease, LiveHubError> {
        if !state.streams.contains_key(&key)
            && state.streams.len() >= self.shared.limits.max_streams
        {
            return Err(LiveHubError::StreamLimitReached {
                maximum: self.shared.limits.max_streams,
            });
        }

        let incarnation = PublisherIncarnation(
            state
                .next_publisher_incarnation
                .checked_add(1)
                .ok_or(LiveHubError::IdentityExhausted)?,
        );
        state.next_publisher_incarnation = incarnation.0;
        let released = {
            let stream = state.streams.entry(key.clone()).or_default();
            stream.publisher = Some(incarnation);
            stream.cache = MediaCache::default();
            reset_subscribers(stream)
        };
        state.fanout_bytes -= released;

        Ok(PublisherLease {
            shared: Arc::clone(&self.shared),
            key,
            incarnation,
        })
    }

    /// Attaches an idle-capable viewer to an exact stream key.
    ///
    /// # Errors
    ///
    /// Returns an error when a stream, service subscriber, per-stream subscriber, or identity cap
    /// is reached.
    pub fn subscribe(&self, key: StreamKey) -> Result<PlaybackSubscription, LiveHubError> {
        let mut state = self.lock();
        if state.subscriber_count >= self.shared.limits.max_subscribers {
            return Err(LiveHubError::SubscriberLimitReached {
                maximum: self.shared.limits.max_subscribers,
            });
        }
        if !state.streams.contains_key(&key)
            && state.streams.len() >= self.shared.limits.max_streams
        {
            return Err(LiveHubError::StreamLimitReached {
                maximum: self.shared.limits.max_streams,
            });
        }
        if state.streams.get(&key).is_some_and(|stream| {
            stream.subscribers.len() >= self.shared.limits.max_subscribers_per_stream
        }) {
            return Err(LiveHubError::StreamSubscriberLimitReached {
                key,
                maximum: self.shared.limits.max_subscribers_per_stream,
            });
        }

        let subscriber_id = state
            .next_subscriber_id
            .checked_add(1)
            .ok_or(LiveHubError::IdentityExhausted)?;
        state.next_subscriber_id = subscriber_id;
        let stream = state.streams.entry(key.clone()).or_default();
        let video_expected = stream.cache.expects_video();
        stream
            .subscribers
            .insert(subscriber_id, SubscriberQueue::new(video_expected));
        state.subscriber_count += 1;

        Ok(PlaybackSubscription {
            shared: Arc::clone(&self.shared),
            key,
            subscriber_id,
        })
    }

    #[must_use]
    pub fn stats(&self) -> LiveHubStats {
        let state = self.lock();
        LiveHubStats {
            streams: state.streams.len(),
            publishers: state
                .streams
                .values()
                .filter(|stream| stream.publisher.is_some())
                .count(),
            subscribers: state.subscriber_count,
            fanout_bytes: state.fanout_bytes,
        }
    }

    #[must_use]
    pub fn limits(&self) -> LiveHubLimits {
        self.shared.limits
    }

    #[must_use]
    pub fn has_publisher(&self, key: &StreamKey) -> bool {
        self.lock()
            .streams
            .get(key)
            .is_some_and(|stream| stream.publisher.is_some())
    }

    fn lock(&self) -> MutexGuard<'_, LiveHubState> {
        self.shared.lock()
    }

    pub(crate) fn lock_roles(&self) -> MutexGuard<'_, ()> {
        self.shared
            .roles
            .lock()
            .expect("live role transaction mutex poisoned")
    }
}
