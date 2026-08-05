use std::io::{self, Seek, SeekFrom, Write};

const FLV_HEADER: [u8; 13] = [
    b'F', b'L', b'V', 1, 0, 0, 0, 0, 9, // FLV header
    0, 0, 0, 0, // PreviousTagSize0
];
const FLV_TAG_HEADER_SIZE: u32 = 11;
const AUDIO_TAG_TYPE: u8 = 8;
const VIDEO_TAG_TYPE: u8 = 9;
const METADATA_TAG_TYPE: u8 = 18;
const AUDIO_FLAG: u8 = 0b0000_0100;
const VIDEO_FLAG: u8 = 0b0000_0001;
const AAC_CODEC_ID: u8 = 10;
const AVC_CODEC_ID: u8 = 7;
const KEYFRAME_TYPE: u8 = 1;
const TIMESTAMP_HALF_RANGE: u32 = 1 << 31;

/// Largest payload representable by an FLV tag's 24-bit data-size field.
pub const MAX_FLV_TAG_DATA_SIZE: usize = 0x00ff_ffff;
/// Largest AAC or AVC sequence header retained while the muxer waits for eligible media.
pub const MAX_CACHED_CODEC_HEADER_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlvTagType {
    Audio,
    Video,
    Metadata,
}

impl std::fmt::Display for FlvTagType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Audio => formatter.write_str("audio"),
            Self::Video => formatter.write_str("video"),
            Self::Metadata => formatter.write_str("metadata"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FlvMuxerError {
    #[error("malformed FLV {tag_type} payload")]
    MalformedPayload { tag_type: FlvTagType },
    #[error(
        "FLV {tag_type} payload is {size} bytes; maximum tag payload is {MAX_FLV_TAG_DATA_SIZE} bytes"
    )]
    PayloadTooLarge { tag_type: FlvTagType, size: usize },
    #[error(
        "FLV {tag_type} codec header is {size} bytes; maximum cached codec header is {MAX_CACHED_CODEC_HEADER_SIZE} bytes"
    )]
    CodecHeaderTooLarge { tag_type: FlvTagType, size: usize },
    #[error("FLV output failed: {0}")]
    Io(#[from] io::Error),
}

struct CachedCodecHeader {
    tag_type: FlvTagType,
    payload: Vec<u8>,
}

enum AudioPacketKind {
    AacSequenceHeader,
    AacRaw,
    Media,
}

enum VideoPacketKind {
    AvcSequenceHeader,
    AvcKeyframe,
    AvcOther,
    Media,
}

/// Incrementally writes RTMP audio and video payloads as an FLV version 1 stream.
///
/// AAC and AVC sequence headers are retained until the first media tag is eligible. Once AVC is
/// observed, media is discarded until an AVC keyframe arrives. Closing the muxer patches the FLV
/// audio/video flags to describe the tags that were actually written.
pub struct FlvMuxer<W> {
    writer: W,
    header_start: u64,
    flags: u8,
    timestamp_base_ms: u32,
    first_media_timestamp_ms: Option<u32>,
    cached_codec_headers: Vec<CachedCodecHeader>,
    aac_sequence_header_seen: bool,
    avc_sequence_header_seen: bool,
    waiting_for_avc_keyframe: bool,
}

impl<W> FlvMuxer<W>
where
    W: Write + Seek,
{
    /// Writes an FLV version 1 header and `PreviousTagSize0` to `writer`.
    ///
    /// # Errors
    ///
    /// Returns an error if the writer position cannot be read or the header cannot be written.
    pub fn new(mut writer: W) -> Result<Self, FlvMuxerError> {
        let header_start = writer.stream_position()?;
        writer.write_all(&FLV_HEADER)?;

        Ok(Self {
            writer,
            header_start,
            flags: 0,
            timestamp_base_ms: 0,
            first_media_timestamp_ms: None,
            cached_codec_headers: Vec::with_capacity(2),
            aac_sequence_header_seen: false,
            avc_sequence_header_seen: false,
            waiting_for_avc_keyframe: false,
        })
    }

    /// Continues a validated FLV stream whose writer is positioned after its last complete tag.
    pub(crate) fn resume(writer: W, flags: u8, last_timestamp_ms: u32) -> Self {
        Self {
            writer,
            header_start: 0,
            flags,
            timestamp_base_ms: last_timestamp_ms,
            first_media_timestamp_ms: None,
            cached_codec_headers: Vec::with_capacity(2),
            aac_sequence_header_seen: false,
            avc_sequence_header_seen: false,
            waiting_for_avc_keyframe: false,
        }
    }

    /// Accepts one immutable RTMP audio-message payload at its RTMP timestamp.
    ///
    /// AAC sequence headers are cached before media starts. Audio received while waiting for the
    /// first AVC keyframe is intentionally discarded.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed or oversized payload, or if output fails.
    pub fn write_audio(&mut self, timestamp_ms: u32, payload: &[u8]) -> Result<(), FlvMuxerError> {
        validate_size(FlvTagType::Audio, payload)?;

        match classify_audio(payload)? {
            AudioPacketKind::AacSequenceHeader => {
                validate_codec_header_size(FlvTagType::Audio, payload)?;
                self.aac_sequence_header_seen = true;
                if self.first_media_timestamp_ms.is_none() {
                    self.cache_codec_header(FlvTagType::Audio, payload);
                    Ok(())
                } else {
                    self.write_normalized_tag(FlvTagType::Audio, timestamp_ms, payload)
                }
            }
            AudioPacketKind::AacRaw if !self.aac_sequence_header_seen => Ok(()),
            AudioPacketKind::AacRaw | AudioPacketKind::Media if self.waiting_for_avc_keyframe => {
                Ok(())
            }
            AudioPacketKind::AacRaw | AudioPacketKind::Media => {
                self.start_if_needed(timestamp_ms)?;
                self.write_normalized_tag(FlvTagType::Audio, timestamp_ms, payload)
            }
        }
    }

    /// Accepts one immutable RTMP video-message payload at its RTMP timestamp.
    ///
    /// AVC sequence headers are cached before media starts. AVC interframes and concurrent audio
    /// are discarded until the first AVC keyframe arrives.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed or oversized payload, or if output fails.
    pub fn write_video(&mut self, timestamp_ms: u32, payload: &[u8]) -> Result<(), FlvMuxerError> {
        validate_size(FlvTagType::Video, payload)?;

        match classify_video(payload)? {
            VideoPacketKind::AvcSequenceHeader => {
                validate_codec_header_size(FlvTagType::Video, payload)?;
                self.avc_sequence_header_seen = true;
                self.waiting_for_avc_keyframe = true;
                if self.first_media_timestamp_ms.is_none() {
                    self.cache_codec_header(FlvTagType::Video, payload);
                    Ok(())
                } else {
                    self.write_normalized_tag(FlvTagType::Video, timestamp_ms, payload)
                }
            }
            VideoPacketKind::AvcKeyframe if !self.avc_sequence_header_seen => Ok(()),
            VideoPacketKind::AvcKeyframe => {
                self.waiting_for_avc_keyframe = false;
                self.start_if_needed(timestamp_ms)?;
                self.write_normalized_tag(FlvTagType::Video, timestamp_ms, payload)
            }
            VideoPacketKind::AvcOther
                if !self.avc_sequence_header_seen || self.waiting_for_avc_keyframe =>
            {
                Ok(())
            }
            VideoPacketKind::AvcOther => {
                self.write_normalized_tag(FlvTagType::Video, timestamp_ms, payload)
            }
            VideoPacketKind::Media if self.waiting_for_avc_keyframe => Ok(()),
            VideoPacketKind::Media => {
                self.start_if_needed(timestamp_ms)?;
                self.write_normalized_tag(FlvTagType::Video, timestamp_ms, payload)
            }
        }
    }

    /// Writes one RTMP metadata payload as an FLV script-data tag at timestamp zero.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized payload or if output fails.
    pub fn write_metadata(&mut self, payload: &[u8]) -> Result<(), FlvMuxerError> {
        validate_size(FlvTagType::Metadata, payload)?;
        self.write_tag(FlvTagType::Metadata, self.timestamp_base_ms, payload)
    }

    /// Patches the FLV audio/video flags, flushes the output, and returns the writer.
    ///
    /// # Errors
    ///
    /// Returns an error if seeking, patching the header, restoring the output position, or flushing
    /// fails.
    pub fn close(mut self) -> Result<W, FlvMuxerError> {
        let end = self.writer.stream_position()?;
        self.writer.seek(SeekFrom::Start(self.header_start + 4))?;
        self.writer.write_all(&[self.flags])?;
        self.writer.seek(SeekFrom::Start(end))?;
        self.writer.flush()?;
        Ok(self.writer)
    }

    pub(crate) fn projected_audio_size(&self, payload: &[u8]) -> Result<u64, FlvMuxerError> {
        validate_size(FlvTagType::Audio, payload)?;
        let size = match classify_audio(payload)? {
            AudioPacketKind::AacSequenceHeader if self.first_media_timestamp_ms.is_none() => 0,
            AudioPacketKind::AacRaw if !self.aac_sequence_header_seen => 0,
            AudioPacketKind::AacRaw | AudioPacketKind::Media if self.waiting_for_avc_keyframe => 0,
            AudioPacketKind::AacRaw | AudioPacketKind::Media
                if self.first_media_timestamp_ms.is_none() =>
            {
                self.cached_codec_header_size()
                    .saturating_add(tag_size(payload))
            }
            AudioPacketKind::AacRaw
            | AudioPacketKind::Media
            | AudioPacketKind::AacSequenceHeader => tag_size(payload),
        };
        Ok(size)
    }

    pub(crate) fn projected_video_size(&self, payload: &[u8]) -> Result<u64, FlvMuxerError> {
        validate_size(FlvTagType::Video, payload)?;
        let size = match classify_video(payload)? {
            VideoPacketKind::AvcSequenceHeader if self.first_media_timestamp_ms.is_none() => 0,
            VideoPacketKind::AvcKeyframe if !self.avc_sequence_header_seen => 0,
            VideoPacketKind::AvcOther
                if !self.avc_sequence_header_seen || self.waiting_for_avc_keyframe =>
            {
                0
            }
            VideoPacketKind::Media if self.waiting_for_avc_keyframe => 0,
            VideoPacketKind::AvcKeyframe
            | VideoPacketKind::AvcOther
            | VideoPacketKind::Media if self.first_media_timestamp_ms.is_none() => self
                .cached_codec_header_size()
                .saturating_add(tag_size(payload)),
            VideoPacketKind::AvcSequenceHeader
            | VideoPacketKind::AvcKeyframe
            | VideoPacketKind::AvcOther
            | VideoPacketKind::Media => tag_size(payload),
        };
        Ok(size)
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn projected_metadata_size(&self, payload: &[u8]) -> Result<u64, FlvMuxerError> {
        validate_size(FlvTagType::Metadata, payload)?;
        Ok(tag_size(payload))
    }

    pub(crate) fn output_position(&mut self) -> Result<u64, FlvMuxerError> {
        Ok(self.writer.stream_position()?)
    }

    pub(crate) const fn has_media(&self) -> bool {
        self.first_media_timestamp_ms.is_some()
    }

    fn cache_codec_header(&mut self, tag_type: FlvTagType, payload: &[u8]) {
        if let Some(index) = self
            .cached_codec_headers
            .iter()
            .position(|header| header.tag_type == tag_type)
        {
            self.cached_codec_headers.remove(index);
        }
        self.cached_codec_headers.push(CachedCodecHeader {
            tag_type,
            payload: payload.to_vec(),
        });
    }

    fn cached_codec_header_size(&self) -> u64 {
        self.cached_codec_headers
            .iter()
            .map(|header| tag_size(&header.payload))
            .sum()
    }

    fn start_if_needed(&mut self, timestamp_ms: u32) -> Result<(), FlvMuxerError> {
        if self.first_media_timestamp_ms.is_some() {
            return Ok(());
        }

        self.first_media_timestamp_ms = Some(timestamp_ms);
        for header in std::mem::take(&mut self.cached_codec_headers) {
            self.write_tag(header.tag_type, self.timestamp_base_ms, &header.payload)?;
        }
        Ok(())
    }

    fn write_normalized_tag(
        &mut self,
        tag_type: FlvTagType,
        timestamp_ms: u32,
        payload: &[u8],
    ) -> Result<(), FlvMuxerError> {
        let first_timestamp = self
            .first_media_timestamp_ms
            .expect("tags are written only after media starts");
        self.write_tag(
            tag_type,
            self.timestamp_base_ms
                .saturating_add(relative_timestamp(first_timestamp, timestamp_ms)),
            payload,
        )
    }

    fn write_tag(
        &mut self,
        tag_type: FlvTagType,
        timestamp_ms: u32,
        payload: &[u8],
    ) -> Result<(), FlvMuxerError> {
        let data_size =
            u32::try_from(payload.len()).expect("validated FLV payload sizes always fit in a u32");
        let data_size_bytes = data_size.to_be_bytes();
        let timestamp_bytes = timestamp_ms.to_be_bytes();
        let mut header = [0; FLV_TAG_HEADER_SIZE as usize];

        header[0] = match tag_type {
            FlvTagType::Audio => AUDIO_TAG_TYPE,
            FlvTagType::Video => VIDEO_TAG_TYPE,
            FlvTagType::Metadata => METADATA_TAG_TYPE,
        };
        header[1..4].copy_from_slice(&data_size_bytes[1..4]);
        header[4..7].copy_from_slice(&timestamp_bytes[1..4]);
        header[7] = timestamp_bytes[0];

        self.writer.write_all(&header)?;
        self.writer.write_all(payload)?;
        self.writer
            .write_all(&(FLV_TAG_HEADER_SIZE + data_size).to_be_bytes())?;
        self.flags |= match tag_type {
            FlvTagType::Audio => AUDIO_FLAG,
            FlvTagType::Video => VIDEO_FLAG,
            FlvTagType::Metadata => 0,
        };
        Ok(())
    }
}

fn validate_size(tag_type: FlvTagType, payload: &[u8]) -> Result<(), FlvMuxerError> {
    if payload.len() > MAX_FLV_TAG_DATA_SIZE {
        return Err(FlvMuxerError::PayloadTooLarge {
            tag_type,
            size: payload.len(),
        });
    }
    Ok(())
}

fn tag_size(payload: &[u8]) -> u64 {
    u64::try_from(payload.len())
        .expect("validated FLV payload length fits in u64")
        .saturating_add(u64::from(FLV_TAG_HEADER_SIZE) + 4)
}

fn validate_codec_header_size(tag_type: FlvTagType, payload: &[u8]) -> Result<(), FlvMuxerError> {
    if payload.len() > MAX_CACHED_CODEC_HEADER_SIZE {
        return Err(FlvMuxerError::CodecHeaderTooLarge {
            tag_type,
            size: payload.len(),
        });
    }
    Ok(())
}

fn classify_audio(payload: &[u8]) -> Result<AudioPacketKind, FlvMuxerError> {
    let Some(sound_header) = payload.first() else {
        return Err(FlvMuxerError::MalformedPayload {
            tag_type: FlvTagType::Audio,
        });
    };
    if sound_header >> 4 != AAC_CODEC_ID {
        return Ok(AudioPacketKind::Media);
    }

    match payload.get(1) {
        Some(0) if payload.len() > 2 => Ok(AudioPacketKind::AacSequenceHeader),
        Some(1) => Ok(AudioPacketKind::AacRaw),
        Some(_) | None => Err(FlvMuxerError::MalformedPayload {
            tag_type: FlvTagType::Audio,
        }),
    }
}

fn classify_video(payload: &[u8]) -> Result<VideoPacketKind, FlvMuxerError> {
    let Some(video_header) = payload.first() else {
        return Err(FlvMuxerError::MalformedPayload {
            tag_type: FlvTagType::Video,
        });
    };
    let frame_type = video_header >> 4;
    let codec_id = video_header & 0x0f;
    if !(1..=5).contains(&frame_type) || !(1..=7).contains(&codec_id) {
        return Err(FlvMuxerError::MalformedPayload {
            tag_type: FlvTagType::Video,
        });
    }
    if codec_id != AVC_CODEC_ID {
        return Ok(VideoPacketKind::Media);
    }
    if payload.len() < 5 {
        return Err(FlvMuxerError::MalformedPayload {
            tag_type: FlvTagType::Video,
        });
    }

    match payload[1] {
        0 if payload.len() > 5 => Ok(VideoPacketKind::AvcSequenceHeader),
        1 if payload.len() > 5 && frame_type == KEYFRAME_TYPE => Ok(VideoPacketKind::AvcKeyframe),
        1 if payload.len() > 5 => Ok(VideoPacketKind::AvcOther),
        2 => Ok(VideoPacketKind::AvcOther),
        _ => Err(FlvMuxerError::MalformedPayload {
            tag_type: FlvTagType::Video,
        }),
    }
}

fn relative_timestamp(first_timestamp_ms: u32, timestamp_ms: u32) -> u32 {
    let elapsed = timestamp_ms.wrapping_sub(first_timestamp_ms);
    if elapsed < TIMESTAMP_HALF_RANGE {
        elapsed
    } else {
        0
    }
}
