use rml_rtmp::sessions::StreamMetadata;

use crate::{MediaEvent, MediaEventKind, StreamProfile};

const MINIMUM_MEASUREMENT_MS: u32 = 1_000;
const SOUND_FORMAT_AAC: u8 = 10;
const EXPLICIT_FREQUENCY_INDEX: u8 = 15;
const ESCAPE_OBJECT_TYPE: u8 = 31;
const AAC_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RateWindow {
    first_ms: Option<u32>,
    last_ms: u32,
    frames: u64,
    bytes: u64,
}

impl RateWindow {
    fn observe(&mut self, timestamp_ms: u32, payload_len: usize) {
        self.first_ms.get_or_insert(timestamp_ms);
        self.last_ms = timestamp_ms;
        self.frames = self.frames.saturating_add(1);
        self.bytes = self.bytes.saturating_add(payload_len as u64);
    }

    fn span_ms(&self) -> Option<u64> {
        let span = self.last_ms.checked_sub(self.first_ms?)?;
        (span >= MINIMUM_MEASUREMENT_MS).then_some(u64::from(span))
    }

    fn bitrate_bps(&self) -> Option<u64> {
        Some(self.bytes.saturating_mul(8_000) / self.span_ms()?)
    }

    fn frame_rate_millihertz(&self) -> Option<u32> {
        let intervals = self.frames.saturating_sub(1);
        u32::try_from(intervals.saturating_mul(1_000_000) / self.span_ms()?).ok()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ProfileAccumulator {
    advertised: StreamProfile,
    audio: RateWindow,
    video: RateWindow,
    last_keyframe_ms: Option<u32>,
    keyframe_interval_ms: Option<u32>,
    aac: Option<(u32, u8)>,
}

impl ProfileAccumulator {
    pub(super) fn advertise(&mut self, metadata: &StreamMetadata) {
        self.advertised = StreamProfile {
            video_width: metadata.video_width,
            video_height: metadata.video_height,
            frame_rate_millihertz: metadata.video_frame_rate.map(frame_rate_millihertz),
            video_bitrate_bps: metadata.video_bitrate_kbps.map(kilobits_to_bps),
            audio_bitrate_bps: metadata.audio_bitrate_kbps.map(kilobits_to_bps),
            keyframe_interval_ms: None,
            audio_sample_rate_hz: metadata.audio_sample_rate,
            audio_channels: metadata
                .audio_channels
                .and_then(|channels| u8::try_from(channels).ok()),
        };
    }

    pub(super) fn observe(&mut self, event: &MediaEvent) {
        match event.kind() {
            MediaEventKind::AacSequenceHeader => self.aac = aac_parameters(event.payload()),
            MediaEventKind::Audio => self
                .audio
                .observe(event.timestamp_ms(), event.payload_len()),
            MediaEventKind::VideoKeyframe => {
                self.observe_keyframe(event.timestamp_ms());
                self.video
                    .observe(event.timestamp_ms(), event.payload_len());
            }
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {
                self.video
                    .observe(event.timestamp_ms(), event.payload_len());
            }
            MediaEventKind::Metadata
            | MediaEventKind::VideoMetadata
            | MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {}
        }
    }

    fn observe_keyframe(&mut self, timestamp_ms: u32) {
        let interval = self
            .last_keyframe_ms
            .and_then(|previous| timestamp_ms.checked_sub(previous))
            .filter(|interval| *interval > 0);
        self.keyframe_interval_ms = interval.or(self.keyframe_interval_ms);
        self.last_keyframe_ms = Some(timestamp_ms);
    }

    pub(super) fn profile(&self) -> StreamProfile {
        let advertised = self.advertised;
        StreamProfile {
            video_width: advertised.video_width,
            video_height: advertised.video_height,
            frame_rate_millihertz: self
                .video
                .frame_rate_millihertz()
                .or(advertised.frame_rate_millihertz),
            video_bitrate_bps: self.video.bitrate_bps().or(advertised.video_bitrate_bps),
            audio_bitrate_bps: self.audio.bitrate_bps().or(advertised.audio_bitrate_bps),
            keyframe_interval_ms: self.keyframe_interval_ms,
            audio_sample_rate_hz: self
                .aac
                .map(|(rate, _)| rate)
                .or(advertised.audio_sample_rate_hz),
            audio_channels: self
                .aac
                .map(|(_, channels)| channels)
                .or(advertised.audio_channels),
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn frame_rate_millihertz(frames_per_second: f32) -> u32 {
    (frames_per_second * 1_000.0)
        .round()
        .clamp(0.0, 1_000_000.0) as u32
}

const fn kilobits_to_bps(kilobits: u32) -> u64 {
    kilobits as u64 * 1_000
}

fn aac_parameters(payload: &[u8]) -> Option<(u32, u8)> {
    let [header, _, first, second, ..] = payload else {
        return None;
    };
    if header >> 4 != SOUND_FORMAT_AAC {
        return None;
    }
    let object_type = first >> 3;
    let frequency_index = ((first & 0b111) << 1) | (second >> 7);
    let channels = (second >> 3) & 0b1111;
    if object_type == ESCAPE_OBJECT_TYPE || frequency_index == EXPLICIT_FREQUENCY_INDEX {
        return None;
    }
    let rate = *AAC_SAMPLE_RATES.get(usize::from(frequency_index))?;
    Some((rate, channels))
}
