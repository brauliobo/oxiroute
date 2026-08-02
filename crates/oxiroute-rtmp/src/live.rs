use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

use rml_rtmp::{
    rml_amf0::{Amf0Value, serialize},
    sessions::StreamMetadata,
};

use crate::{MAX_FLV_TAG_DATA_SIZE, StreamKey};

const AAC_CODEC_ID: u8 = 10;
const AVC_CODEC_ID: u8 = 7;
const ENHANCED_VIDEO_HEADER: u8 = 0x80;

/// Immutable classification of one RTMP media message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaEventKind {
    Metadata,
    AacSequenceHeader,
    AvcSequenceHeader,
    HevcSequenceHeader,
    Av1SequenceHeader,
    Audio,
    VideoKeyframe,
    VideoInterframe,
    VideoDisposable,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum VideoCodec {
    Avc,
    Hevc,
    Av1,
}

impl VideoCodec {
    #[must_use]
    pub const fn flv_codec_id(self) -> Option<u8> {
        match self {
            Self::Avc => Some(AVC_CODEC_ID),
            Self::Hevc | Self::Av1 => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoCodecIdentifier {
    Flv(u8),
    FourCc([u8; 4]),
}

impl VideoCodecIdentifier {
    #[must_use]
    pub fn codec(self) -> Option<VideoCodec> {
        match self {
            Self::Flv(AVC_CODEC_ID) => Some(VideoCodec::Avc),
            Self::FourCc(four_cc) if four_cc == *b"avc1" => Some(VideoCodec::Avc),
            Self::FourCc(four_cc) if four_cc == *b"hvc1" => Some(VideoCodec::Hevc),
            Self::FourCc(four_cc) if four_cc == *b"av01" => Some(VideoCodec::Av1),
            Self::Flv(_) | Self::FourCc(_) => None,
        }
    }

    #[must_use]
    pub const fn flv_codec_id(self) -> Option<u8> {
        match self {
            Self::Flv(codec_id) => Some(codec_id),
            Self::FourCc(_) => None,
        }
    }

    #[must_use]
    pub const fn four_cc(self) -> Option<[u8; 4]> {
        match self {
            Self::FourCc(four_cc) => Some(four_cc),
            Self::Flv(_) => None,
        }
    }

    #[must_use]
    pub const fn recording_supported(self) -> bool {
        matches!(self, Self::Flv(AVC_CODEC_ID))
    }
}

impl std::fmt::Display for VideoCodecIdentifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flv(codec_id) => write!(formatter, "FLV codec {codec_id}"),
            Self::FourCc(four_cc) => write!(formatter, "FourCC {}", four_cc.escape_ascii()),
        }
    }
}

/// One immutable RTMP media message shared between bounded playback queues.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaEvent {
    kind: MediaEventKind,
    timestamp_ms: u32,
    payload: Arc<[u8]>,
    metadata: Option<Arc<StreamMetadata>>,
    video_codec: Option<VideoCodec>,
    video_codec_identifier: Option<VideoCodecIdentifier>,
}

impl MediaEvent {
    /// Creates a metadata event.
    ///
    /// # Errors
    ///
    /// Returns an error when metadata cannot be encoded or exceeds the RTMP media-message bound.
    pub fn metadata(metadata: StreamMetadata) -> Result<Self, MediaEventError> {
        let payload = encode_metadata(&metadata)?;
        let mut event = Self::new(MediaEventKind::Metadata, 0, payload.into())?;
        event.metadata = Some(Arc::new(metadata));
        Ok(event)
    }

    /// Classifies an FLV audio-message payload.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed AAC or oversized payloads.
    pub fn audio(
        timestamp_ms: u32,
        payload: impl Into<Arc<[u8]>>,
    ) -> Result<Self, MediaEventError> {
        let payload = payload.into();
        let Some(sound_header) = payload.first() else {
            return Err(MediaEventError::MalformedAudio);
        };
        let kind = if sound_header >> 4 == AAC_CODEC_ID {
            match payload.get(1) {
                Some(0) if payload.len() > 2 => MediaEventKind::AacSequenceHeader,
                Some(1) if payload.len() > 2 => MediaEventKind::Audio,
                Some(_) | None => return Err(MediaEventError::MalformedAudio),
            }
        } else {
            MediaEventKind::Audio
        };
        Self::new(kind, timestamp_ms, payload)
    }

    /// Classifies an FLV video-message payload.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed frame/packet fields, unsupported codecs, or oversized
    /// payloads. Legacy AVC and enhanced AVC (`avc1`), HEVC (`hvc1`), and AV1 (`av01`) are
    /// supported.
    pub fn video(
        timestamp_ms: u32,
        payload: impl Into<Arc<[u8]>>,
    ) -> Result<Self, MediaEventError> {
        let payload = payload.into();
        let Some(video_header) = payload.first().copied() else {
            return Err(MediaEventError::MalformedVideo);
        };
        let (kind, video_codec, video_codec_identifier) =
            if video_header & ENHANCED_VIDEO_HEADER == 0 {
                classify_legacy_video(&payload, video_header)?
            } else {
                classify_enhanced_video(&payload, video_header)?
            };
        let mut event = Self::new(kind, timestamp_ms, payload)?;
        event.video_codec = Some(video_codec);
        event.video_codec_identifier = Some(video_codec_identifier);
        Ok(event)
    }

    #[must_use]
    pub const fn kind(&self) -> MediaEventKind {
        self.kind
    }

    #[must_use]
    pub const fn timestamp_ms(&self) -> u32 {
        self.timestamp_ms
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[must_use]
    pub fn payload_arc(&self) -> Arc<[u8]> {
        Arc::clone(&self.payload)
    }

    #[must_use]
    pub fn payload_len(&self) -> usize {
        self.payload.len()
    }

    #[must_use]
    pub fn stream_metadata(&self) -> Option<&StreamMetadata> {
        self.metadata.as_deref()
    }

    #[must_use]
    pub const fn video_codec(&self) -> Option<VideoCodec> {
        self.video_codec
    }

    #[must_use]
    pub const fn video_codec_identifier(&self) -> Option<VideoCodecIdentifier> {
        self.video_codec_identifier
    }

    fn new(
        kind: MediaEventKind,
        timestamp_ms: u32,
        payload: Arc<[u8]>,
    ) -> Result<Self, MediaEventError> {
        if payload.len() > MAX_FLV_TAG_DATA_SIZE {
            return Err(MediaEventError::PayloadTooLarge {
                size: payload.len(),
                maximum: MAX_FLV_TAG_DATA_SIZE,
            });
        }
        Ok(Self {
            kind,
            timestamp_ms,
            payload,
            metadata: None,
            video_codec: None,
            video_codec_identifier: None,
        })
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum MediaEventError {
    #[error("malformed FLV audio payload")]
    MalformedAudio,
    #[error("malformed FLV video payload")]
    MalformedVideo,
    #[error("unsupported video codec {0}")]
    UnsupportedVideoCodec(VideoCodecIdentifier),
    #[error("RTMP metadata cannot be encoded: {0}")]
    MetadataEncoding(String),
    #[error("media payload is {size} bytes; maximum is {maximum} bytes")]
    PayloadTooLarge { size: usize, maximum: usize },
}

fn classify_legacy_video(
    payload: &[u8],
    video_header: u8,
) -> Result<(MediaEventKind, VideoCodec, VideoCodecIdentifier), MediaEventError> {
    let frame_kind = classify_video_frame(video_header >> 4)?;
    let codec_id = video_header & 0x0f;
    if codec_id != AVC_CODEC_ID {
        return Err(MediaEventError::UnsupportedVideoCodec(
            VideoCodecIdentifier::Flv(codec_id),
        ));
    }
    let kind = match payload.get(1) {
        Some(0) if payload.len() > 5 && frame_kind == MediaEventKind::VideoKeyframe => {
            MediaEventKind::AvcSequenceHeader
        }
        Some(1) if payload.len() > 5 => frame_kind,
        Some(_) | None => return Err(MediaEventError::MalformedVideo),
    };
    Ok((kind, VideoCodec::Avc, VideoCodecIdentifier::Flv(codec_id)))
}

fn classify_enhanced_video(
    payload: &[u8],
    video_header: u8,
) -> Result<(MediaEventKind, VideoCodec, VideoCodecIdentifier), MediaEventError> {
    if payload.len() <= 5 {
        return Err(MediaEventError::MalformedVideo);
    }
    let frame_kind = classify_video_frame((video_header >> 4) & 0x07)?;
    let four_cc: [u8; 4] = payload[1..5]
        .try_into()
        .expect("the enhanced video header length was checked");
    let codec = match &four_cc {
        b"avc1" => VideoCodec::Avc,
        b"hvc1" => VideoCodec::Hevc,
        b"av01" => VideoCodec::Av1,
        _ => {
            return Err(MediaEventError::UnsupportedVideoCodec(
                VideoCodecIdentifier::FourCc(four_cc),
            ));
        }
    };
    let kind = match video_header & 0x0f {
        0 if frame_kind == MediaEventKind::VideoKeyframe => match codec {
            VideoCodec::Avc => MediaEventKind::AvcSequenceHeader,
            VideoCodec::Hevc => MediaEventKind::HevcSequenceHeader,
            VideoCodec::Av1 => MediaEventKind::Av1SequenceHeader,
        },
        1 | 3 => frame_kind,
        _ => return Err(MediaEventError::MalformedVideo),
    };
    Ok((kind, codec, VideoCodecIdentifier::FourCc(four_cc)))
}

fn classify_video_frame(frame_type: u8) -> Result<MediaEventKind, MediaEventError> {
    match frame_type {
        1 | 4 => Ok(MediaEventKind::VideoKeyframe),
        2 => Ok(MediaEventKind::VideoInterframe),
        3 => Ok(MediaEventKind::VideoDisposable),
        _ => Err(MediaEventError::MalformedVideo),
    }
}

fn encode_metadata(metadata: &StreamMetadata) -> Result<Vec<u8>, MediaEventError> {
    let mut properties = HashMap::with_capacity(11);
    if let Some(value) = metadata.video_width {
        properties.insert("width".to_owned(), Amf0Value::Number(f64::from(value)));
    }
    if let Some(value) = metadata.video_height {
        properties.insert("height".to_owned(), Amf0Value::Number(f64::from(value)));
    }
    if let Some(value) = metadata.video_codec_id {
        properties.insert(
            "videocodecid".to_owned(),
            Amf0Value::Number(f64::from(value)),
        );
    }
    if let Some(value) = metadata.video_frame_rate {
        properties.insert("framerate".to_owned(), Amf0Value::Number(f64::from(value)));
    }
    if let Some(value) = metadata.video_bitrate_kbps {
        properties.insert(
            "videodatarate".to_owned(),
            Amf0Value::Number(f64::from(value)),
        );
    }
    if let Some(value) = metadata.audio_codec_id {
        properties.insert(
            "audiocodecid".to_owned(),
            Amf0Value::Number(f64::from(value)),
        );
    }
    if let Some(value) = metadata.audio_bitrate_kbps {
        properties.insert(
            "audiodatarate".to_owned(),
            Amf0Value::Number(f64::from(value)),
        );
    }
    if let Some(value) = metadata.audio_sample_rate {
        properties.insert(
            "audiosamplerate".to_owned(),
            Amf0Value::Number(f64::from(value)),
        );
    }
    if let Some(value) = metadata.audio_channels {
        properties.insert(
            "audiochannels".to_owned(),
            Amf0Value::Number(f64::from(value)),
        );
    }
    if let Some(value) = metadata.audio_is_stereo {
        properties.insert("stereo".to_owned(), Amf0Value::Boolean(value));
    }
    if let Some(value) = &metadata.encoder {
        properties.insert("encoder".to_owned(), Amf0Value::Utf8String(value.clone()));
    }

    serialize(&vec![
        Amf0Value::Utf8String("onMetaData".to_owned()),
        Amf0Value::Object(properties),
    ])
    .map_err(|error| MediaEventError::MetadataEncoding(error.to_string()))
}

/// Explicit service and per-viewer bounds for a [`LiveHub`].
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

/// Current resource use by a [`LiveHub`].
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

/// Independent bounded live-stream fanout state.
#[derive(Clone)]
pub struct LiveHub {
    shared: Arc<LiveHubShared>,
}

struct LiveHubShared {
    limits: LiveHubLimits,
    roles: Mutex<()>,
    state: Mutex<LiveHubState>,
}

#[derive(Default)]
struct LiveHubState {
    streams: BTreeMap<StreamKey, LiveStream>,
    subscriber_count: usize,
    fanout_bytes: usize,
    next_publisher_incarnation: u64,
    next_subscriber_id: u64,
}

#[derive(Default)]
struct LiveStream {
    publisher: Option<PublisherIncarnation>,
    cache: MediaCache,
    subscribers: BTreeMap<u64, SubscriberQueue>,
}

#[derive(Default)]
struct MediaCache {
    metadata: Option<MediaEvent>,
    aac_sequence_header: Option<MediaEvent>,
    video_sequence_headers: BTreeMap<VideoCodec, MediaEvent>,
    video_track: CurrentVideoTrack,
}

impl MediaCache {
    fn transition(&mut self, event: &MediaEvent) -> VideoTrackTransition {
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

    fn store(&mut self, event: MediaEvent) {
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

    fn mixed_bootstrap(&self, keyframe: &MediaEvent) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(4);
        events.extend(self.metadata.iter().cloned());
        events.extend(self.aac_sequence_header.iter().cloned());
        events.extend(self.video_sequence_header(keyframe).cloned());
        events.push(keyframe.clone());
        events
    }

    fn audio_bootstrap(&self, audio: &MediaEvent) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(3);
        events.extend(self.metadata.iter().cloned());
        events.extend(self.aac_sequence_header.iter().cloned());
        events.push(audio.clone());
        events
    }

    fn video_bootstrap(&self, keyframe: &MediaEvent) -> Vec<MediaEvent> {
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
enum VideoTrackTransition {
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
enum SubscriberState {
    AwaitingMedia,
    AwaitingKeyframe,
    Audio,
    Mixed,
}

struct SubscriberQueue {
    events: VecDeque<MediaEvent>,
    bytes: usize,
    state: SubscriberState,
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

/// RAII ownership of one publisher incarnation.
pub struct PublisherLease {
    shared: Arc<LiveHubShared>,
    key: StreamKey,
    incarnation: PublisherIncarnation,
}

impl PublisherLease {
    #[must_use]
    pub fn key(&self) -> &StreamKey {
        &self.key
    }

    #[must_use]
    pub const fn incarnation(&self) -> PublisherIncarnation {
        self.incarnation
    }

    /// Fans an event into bounded viewer queues without performing I/O or waiting for capacity.
    ///
    /// # Errors
    ///
    /// Returns an error when this incarnation is stale or a cacheable event exceeds its cache
    /// bound.
    pub fn publish(&self, event: MediaEvent) -> Result<PublishReport, LiveHubError> {
        validate_cache_size(self.shared.limits, &event)?;
        let mut state = self.shared.lock();
        let mut fanout_bytes = state.fanout_bytes;
        let stream = state
            .streams
            .get_mut(&self.key)
            .filter(|stream| stream.publisher == Some(self.incarnation))
            .ok_or_else(|| LiveHubError::PublisherExpired {
                key: self.key.clone(),
            })?;
        let video_transition = stream.cache.transition(&event);
        let mut subscriber_ids: Vec<_> = stream.subscribers.keys().copied().collect();
        subscriber_ids.sort_by_key(|subscriber_id| {
            let subscriber = &stream.subscribers[subscriber_id];
            (subscriber.bytes, *subscriber_id)
        });
        let mut report = PublishReport::default();
        for subscriber_id in subscriber_ids {
            if let Some(subscriber) = stream.subscribers.get_mut(&subscriber_id) {
                apply_video_transition(
                    subscriber,
                    video_transition,
                    &mut fanout_bytes,
                    &mut report,
                );
                fanout_event(
                    subscriber,
                    &stream.cache,
                    &event,
                    self.shared.limits,
                    &mut fanout_bytes,
                    &mut report,
                );
            }
        }
        report.stream_fanout_bytes = stream
            .subscribers
            .values()
            .map(|subscriber| subscriber.bytes)
            .sum();
        stream.cache.store(event);
        state.fanout_bytes = fanout_bytes;
        Ok(report)
    }
}

impl Drop for PublisherLease {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        let Some(stream) = state.streams.get_mut(&self.key) else {
            return;
        };
        if stream.publisher != Some(self.incarnation) {
            return;
        }

        stream.publisher = None;
        stream.cache = MediaCache::default();
        let released = reset_subscribers(stream);
        let remove_stream = stream.subscribers.is_empty();
        state.fanout_bytes -= released;
        if remove_stream {
            state.streams.remove(&self.key);
        }
    }
}

/// Pull-only handle for one bounded playback queue.
pub struct PlaybackSubscription {
    shared: Arc<LiveHubShared>,
    key: StreamKey,
    subscriber_id: u64,
}

impl PlaybackSubscription {
    #[must_use]
    pub fn key(&self) -> &StreamKey {
        &self.key
    }

    /// Removes and returns the next queued event without performing I/O.
    #[must_use]
    pub fn try_next(&self) -> Option<MediaEvent> {
        let mut state = self.shared.lock();
        let (event, bytes) = {
            let subscriber = state
                .streams
                .get_mut(&self.key)?
                .subscribers
                .get_mut(&self.subscriber_id)?;
            let event = subscriber.events.pop_front()?;
            subscriber.bytes -= event.payload_len();
            let bytes = event.payload_len();
            (event, bytes)
        };
        state.fanout_bytes -= bytes;
        Some(event)
    }

    #[must_use]
    pub fn queued_messages(&self) -> usize {
        self.with_queue(|subscriber| subscriber.events.len())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        self.with_queue(|subscriber| subscriber.bytes).unwrap_or(0)
    }

    #[must_use]
    pub fn is_waiting_for_keyframe(&self) -> bool {
        self.with_queue(|subscriber| {
            matches!(
                subscriber.state,
                SubscriberState::AwaitingMedia | SubscriberState::AwaitingKeyframe
            )
        })
        .unwrap_or(true)
    }

    fn with_queue<T>(&self, inspect: impl FnOnce(&SubscriberQueue) -> T) -> Option<T> {
        let state = self.shared.lock();
        state
            .streams
            .get(&self.key)?
            .subscribers
            .get(&self.subscriber_id)
            .map(inspect)
    }
}

impl Drop for PlaybackSubscription {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        let Some(stream) = state.streams.get_mut(&self.key) else {
            return;
        };
        let Some(subscriber) = stream.subscribers.remove(&self.subscriber_id) else {
            return;
        };
        let remove_stream = stream.publisher.is_none() && stream.subscribers.is_empty();
        state.subscriber_count -= 1;
        state.fanout_bytes -= subscriber.bytes;
        if remove_stream {
            state.streams.remove(&self.key);
        }
    }
}

impl LiveHubShared {
    fn lock(&self) -> MutexGuard<'_, LiveHubState> {
        self.state.lock().expect("live hub mutex poisoned")
    }
}

fn validate_cache_size(limits: LiveHubLimits, event: &MediaEvent) -> Result<(), LiveHubError> {
    let maximum = match event.kind() {
        MediaEventKind::Metadata => limits.max_cached_metadata_bytes,
        MediaEventKind::AacSequenceHeader
        | MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader => limits.max_cached_codec_header_bytes,
        MediaEventKind::Audio
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => return Ok(()),
    };
    if event.payload_len() > maximum {
        return Err(LiveHubError::CachedEventTooLarge {
            kind: event.kind(),
            size: event.payload_len(),
            maximum,
        });
    }
    Ok(())
}

fn fanout_event(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    event: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    match subscriber.state {
        SubscriberState::AwaitingMedia => match event.kind() {
            MediaEventKind::Audio => {
                enqueue_audio_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
            MediaEventKind::VideoKeyframe => {
                enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
            MediaEventKind::Metadata
            | MediaEventKind::AacSequenceHeader
            | MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {}
        },
        SubscriberState::AwaitingKeyframe => {
            if event.kind() == MediaEventKind::VideoKeyframe {
                enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
        }
        SubscriberState::Audio => {
            fanout_audio_event(subscriber, cache, event, limits, fanout_bytes, report);
        }
        SubscriberState::Mixed => {
            fanout_mixed_event(subscriber, cache, event, limits, fanout_bytes, report);
        }
    }
}

fn apply_video_transition(
    subscriber: &mut SubscriberQueue,
    transition: VideoTrackTransition,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    match transition {
        VideoTrackTransition::None => {}
        VideoTrackTransition::Absent => match subscriber.state {
            SubscriberState::AwaitingKeyframe => {
                subscriber.state = SubscriberState::AwaitingMedia;
            }
            SubscriberState::Mixed => {
                *fanout_bytes -= clear_queue(subscriber);
                subscriber.state = SubscriberState::AwaitingMedia;
                report.viewers_resynchronized += 1;
            }
            SubscriberState::AwaitingMedia | SubscriberState::Audio => {}
        },
        VideoTrackTransition::Present { codec_changed } => match subscriber.state {
            SubscriberState::AwaitingMedia => {
                subscriber.state = SubscriberState::AwaitingKeyframe;
            }
            SubscriberState::Mixed if codec_changed => {
                *fanout_bytes -= clear_queue(subscriber);
                subscriber.state = SubscriberState::AwaitingKeyframe;
                report.viewers_resynchronized += 1;
            }
            SubscriberState::AwaitingKeyframe | SubscriberState::Audio | SubscriberState::Mixed => {
            }
        },
    }
}

fn fanout_audio_event(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    event: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    match event.kind() {
        MediaEventKind::VideoKeyframe => {
            let bootstrap = cache.video_bootstrap(event);
            if enqueue(subscriber, &bootstrap, limits, fanout_bytes) {
                subscriber.state = SubscriberState::Mixed;
                report.queued_events += bootstrap.len();
                return;
            }
            *fanout_bytes -= clear_queue(subscriber);
            subscriber.state = SubscriberState::AwaitingKeyframe;
            report.dropped_events += 1;
            report.viewers_resynchronized += 1;
            enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
        }
        MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => {}
        MediaEventKind::Metadata | MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => {
            if enqueue(
                subscriber,
                std::slice::from_ref(event),
                limits,
                fanout_bytes,
            ) {
                report.queued_events += 1;
                return;
            }
            *fanout_bytes -= clear_queue(subscriber);
            subscriber.state = SubscriberState::AwaitingMedia;
            report.dropped_events += 1;
            report.viewers_resynchronized += 1;
            if event.kind() == MediaEventKind::Audio {
                enqueue_audio_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
        }
    }
}

fn fanout_mixed_event(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    event: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    if enqueue(
        subscriber,
        std::slice::from_ref(event),
        limits,
        fanout_bytes,
    ) {
        report.queued_events += 1;
        return;
    }
    if event.kind() == MediaEventKind::VideoDisposable {
        report.dropped_events += 1;
        return;
    }

    *fanout_bytes -= clear_queue(subscriber);
    subscriber.state = SubscriberState::AwaitingKeyframe;
    report.dropped_events += 1;
    report.viewers_resynchronized += 1;
    if event.kind() == MediaEventKind::VideoKeyframe {
        enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
    }
}

fn enqueue_audio_bootstrap(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    audio: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    let bootstrap = cache.audio_bootstrap(audio);
    if enqueue(subscriber, &bootstrap, limits, fanout_bytes) {
        subscriber.state = SubscriberState::Audio;
        report.queued_events += bootstrap.len();
    } else {
        report.dropped_events += 1;
    }
}

fn enqueue_mixed_bootstrap(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    keyframe: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    let bootstrap = cache.mixed_bootstrap(keyframe);
    if enqueue(subscriber, &bootstrap, limits, fanout_bytes) {
        subscriber.state = SubscriberState::Mixed;
        report.queued_events += bootstrap.len();
    } else {
        report.dropped_events += 1;
    }
}

fn enqueue(
    subscriber: &mut SubscriberQueue,
    events: &[MediaEvent],
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
) -> bool {
    let Some(message_count) = subscriber.events.len().checked_add(events.len()) else {
        return false;
    };
    let Some(batch_bytes) = events.iter().try_fold(0_usize, |total, event| {
        total.checked_add(event.payload_len())
    }) else {
        return false;
    };
    let Some(queue_bytes) = subscriber.bytes.checked_add(batch_bytes) else {
        return false;
    };
    let Some(total_fanout_bytes) = fanout_bytes.checked_add(batch_bytes) else {
        return false;
    };
    if message_count > limits.max_queue_messages_per_subscriber
        || queue_bytes > limits.max_queue_bytes_per_subscriber
        || total_fanout_bytes > limits.max_fanout_bytes
    {
        return false;
    }

    subscriber.events.extend(events.iter().cloned());
    subscriber.bytes = queue_bytes;
    *fanout_bytes = total_fanout_bytes;
    true
}

fn reset_subscribers(stream: &mut LiveStream) -> usize {
    stream
        .subscribers
        .values_mut()
        .map(|subscriber| {
            subscriber.state = SubscriberState::AwaitingMedia;
            clear_queue(subscriber)
        })
        .sum()
}

fn clear_queue(subscriber: &mut SubscriberQueue) -> usize {
    subscriber.events.clear();
    std::mem::take(&mut subscriber.bytes)
}
