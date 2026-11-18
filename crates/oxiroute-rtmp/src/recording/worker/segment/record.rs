use std::time::{SystemTime, UNIX_EPOCH};

use crate::{MediaEvent, MediaEventKind, RecordingMeta, VideoCodec};

const SOUND_FORMAT_MP3: u8 = 2;
const SOUND_FORMAT_AAC: u8 = 10;

pub(super) struct SegmentRecord {
    stream: String,
    started_unix_ms: u64,
    first_timestamp_ms: Option<u32>,
    last_timestamp_ms: u32,
    video_codec: Option<&'static str>,
    audio_codec: Option<&'static str>,
}

impl SegmentRecord {
    pub(super) fn new(stream: String, started_unix_ms: u64) -> Self {
        Self {
            stream,
            started_unix_ms,
            first_timestamp_ms: None,
            last_timestamp_ms: 0,
            video_codec: None,
            audio_codec: None,
        }
    }

    pub(super) fn observe(&mut self, event: &MediaEvent) {
        match event.kind() {
            MediaEventKind::AacSequenceHeader => self.audio_codec = Some("aac"),
            MediaEventKind::Audio => {
                self.audio_codec = audio_codec_name(event.payload()).or(self.audio_codec);
                self.observe_timestamp(event.timestamp_ms());
            }
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => self.observe_video_codec(event),
            MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {
                self.observe_video_codec(event);
                self.observe_timestamp(event.timestamp_ms());
            }
            MediaEventKind::Metadata => {}
        }
    }

    fn observe_video_codec(&mut self, event: &MediaEvent) {
        self.video_codec = event
            .video_codec_identifier()
            .and_then(crate::VideoCodecIdentifier::codec)
            .map(|codec| match codec {
                VideoCodec::Avc => "avc",
                VideoCodec::Hevc => "hevc",
                VideoCodec::Av1 => "av1",
            })
            .or(self.video_codec);
    }

    fn observe_timestamp(&mut self, timestamp_ms: u32) {
        self.first_timestamp_ms.get_or_insert(timestamp_ms);
        self.last_timestamp_ms = timestamp_ms;
    }

    pub(super) fn meta(&self, complete: bool) -> RecordingMeta {
        RecordingMeta {
            complete,
            stream: Some(self.stream.clone()),
            started_unix_ms: Some(self.started_unix_ms),
            ended_unix_ms: complete.then(now_unix_ms),
            duration_ms: self
                .first_timestamp_ms
                .map(|first| u64::from(self.last_timestamp_ms.saturating_sub(first))),
            video_codec: self.video_codec.map(str::to_owned),
            audio_codec: self.audio_codec.map(str::to_owned),
        }
    }
}

fn audio_codec_name(payload: &[u8]) -> Option<&'static str> {
    match payload.first()? >> 4 {
        SOUND_FORMAT_AAC => Some("aac"),
        SOUND_FORMAT_MP3 => Some("mp3"),
        _ => None,
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
