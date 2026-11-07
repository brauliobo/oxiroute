#[allow(clippy::wildcard_imports)]
use super::*;

const AAC_CODEC_ID: u8 = 10;
pub(super) const AVC_CODEC_ID: u8 = 7;
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
        match self {
            Self::Flv(codec_id) => codec_id == AVC_CODEC_ID,
            Self::FourCc(four_cc) => matches!(
                four_cc,
                [b'a' | b'h', b'v', b'c', b'1'] | [b'a', b'v', b'0', b'1']
            ),
        }
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

    pub(crate) fn from_wire(
        kind: MediaEventKind,
        timestamp_ms: u32,
        payload: Vec<u8>,
        video_codec_identifier: Option<VideoCodecIdentifier>,
    ) -> Result<Self, MediaEventError> {
        let mut event = Self::new(kind, timestamp_ms, payload.into())?;
        event.video_codec = video_codec_identifier.and_then(VideoCodecIdentifier::codec);
        event.video_codec_identifier = video_codec_identifier;
        if kind == MediaEventKind::Metadata {
            event.metadata = decode_metadata(event.payload()).map(Arc::new);
        }
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

fn decode_metadata(payload: &[u8]) -> Option<StreamMetadata> {
    use rml_rtmp::{
        messages::{MessagePayload, RtmpMessage},
        time::RtmpTimestamp,
    };

    let payload = MessagePayload {
        timestamp: RtmpTimestamp::new(0),
        type_id: 18,
        message_stream_id: 0,
        data: Bytes::copy_from_slice(payload),
    };
    let RtmpMessage::Amf0Data { mut values } = payload.to_rtmp_message().ok()? else {
        return None;
    };
    if !matches!(values.first(), Some(Amf0Value::Utf8String(name)) if name == "onMetaData") {
        return None;
    }
    let Amf0Value::Object(properties) = values.get_mut(1)? else {
        return None;
    };
    let mut metadata = StreamMetadata::new();
    metadata.apply_metadata_values(std::mem::take(properties));
    Some(metadata)
}
