use std::{
    collections::VecDeque,
    fmt::Write as _,
    path::{Component, Path, PathBuf},
    str,
    sync::Arc,
    time::Duration,
};

use crate::{
    MediaEvent, MediaEventKind, PublisherIncarnation, StreamKey,
    clock::unix_time_ms,
    media::parser::{parse_aac_configuration, parse_avc_configuration},
    media::segment_window::SegmentWindowConfig,
    media::storage::{MediaStore, MediaStoreError},
};

mod boxes;
mod manifest;
mod parse;

#[allow(clippy::wildcard_imports)]
use boxes::*;
#[allow(clippy::wildcard_imports)]
use manifest::*;
#[cfg(feature = "fuzzing")]
pub(crate) use parse::fuzz_media_configuration;
#[allow(clippy::wildcard_imports)]
use parse::*;

pub(crate) const MAX_DASH_SEGMENTS: usize = 512;
const VIDEO_TIMESCALE: u32 = 90_000;
const DEFAULT_VIDEO_SAMPLE_DURATION: u32 = 3_000;
const AAC_SAMPLES_PER_FRAME: u32 = 1_024;
const MAX_CODEC_PARAMETER_SETS: usize = 4;
const MAX_CODEC_PARAMETER_SET_BYTES: usize = 64 * 1024;
const MAX_DASH_FILENAME_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashSegmentNaming {
    Sequential,
    Timestamp,
    System,
}

#[derive(Clone)]
pub struct DashOutputConfig {
    pub store: Arc<MediaStore>,
    pub segment_duration: Duration,
    pub max_segment_duration: Duration,
    pub playlist_length: Duration,
    pub naming: DashSegmentNaming,
    pub nested: bool,
    pub cleanup: bool,
    pub max_segment_bytes: usize,
    pub max_queue_messages: usize,
}

impl DashOutputConfig {
    fn segment_window(&self) -> SegmentWindowConfig {
        SegmentWindowConfig::new(self.segment_duration, self.max_segment_duration)
    }

    pub(crate) fn media_directory(&self, prefix: &Path) -> PathBuf {
        if self.nested {
            prefix.join("dash")
        } else {
            prefix.to_path_buf()
        }
    }

    fn segment_filename(&self, sequence: u64, timestamp_ms: u32) -> String {
        match self.naming {
            DashSegmentNaming::Sequential => format!("seg-{sequence}.m4s"),
            DashSegmentNaming::Timestamp => format!("seg-{timestamp_ms}-{sequence}.m4s"),
            DashSegmentNaming::System => {
                format!("seg-{}-{sequence}.m4s", unix_time_ms())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AvcConfig {
    profile: u8,
    compatibility: u8,
    level: u8,
    configuration_record: Vec<u8>,
}

impl AvcConfig {
    fn codec_string(&self) -> String {
        format!(
            "avc1.{:02x}{:02x}{:02x}",
            self.profile, self.compatibility, self.level
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AacConfig {
    audio_object_type: u8,
    sample_rate: u32,
    channel_configuration: u8,
    audio_specific_config: Vec<u8>,
}

pub(crate) struct DashSegmenter {
    key: StreamKey,
    incarnation: PublisherIncarnation,
    store: Arc<MediaStore>,
    config: Arc<DashOutputConfig>,
    video_config: Option<AvcConfig>,
    audio_config: Option<AacConfig>,
    video_width: u16,
    video_height: u16,
    current: Option<DashSegmentBuilder>,
    published: VecDeque<PublishedSegment>,
    next_sequence: u64,
    initialization_published: bool,
    failed: bool,
}

impl Drop for DashSegmenter {
    fn drop(&mut self) {
        self.store.close(&self.key, self.incarnation);
    }
}

struct DashSegmentBuilder {
    start_timestamp_ms: u32,
    last_timestamp_ms: u32,
    video: Vec<DashVideoSample>,
    audio: Vec<DashAudioSample>,
}

struct DashVideoSample {
    timestamp_ms: u32,
    composition_time_ms: i32,
    keyframe: bool,
    data: Vec<u8>,
}

struct DashAudioSample {
    timestamp_ms: u32,
    data: Vec<u8>,
}

#[derive(Clone)]
struct PublishedSegment {
    sequence: u64,
    start_timestamp_ms: u32,
    duration_ms: u64,
    path: PathBuf,
}

impl DashSegmenter {
    pub(crate) fn new(
        key: StreamKey,
        incarnation: PublisherIncarnation,
        store: Arc<MediaStore>,
        config: Arc<DashOutputConfig>,
    ) -> Result<Self, MediaStoreError> {
        let prefix = store
            .current_prefix(&key)
            .ok_or(MediaStoreError::StaleIncarnation)?;
        let directory = config.media_directory(&prefix);
        let (published, next_sequence) = load_manifest(&store, &directory)?;
        Ok(Self {
            key,
            incarnation,
            store,
            config,
            video_config: None,
            audio_config: None,
            video_width: 0,
            video_height: 0,
            current: None,
            published,
            next_sequence,
            initialization_published: false,
            failed: false,
        })
    }

    pub(crate) fn accept(&mut self, event: &MediaEvent) {
        if self.failed {
            return;
        }
        match event.kind() {
            MediaEventKind::Metadata => {
                if let Some(metadata) = event.stream_metadata() {
                    self.video_width = metadata
                        .video_width
                        .and_then(|value| u16::try_from(value).ok())
                        .unwrap_or_default();
                    self.video_height = metadata
                        .video_height
                        .and_then(|value| u16::try_from(value).ok())
                        .unwrap_or_default();
                }
            }
            MediaEventKind::AvcSequenceHeader => {
                let Some(config) = parse_avc_config(event.payload()) else {
                    self.failed = true;
                    return;
                };
                if self
                    .video_config
                    .as_ref()
                    .is_some_and(|current| current != &config)
                {
                    self.failed = true;
                    return;
                }
                self.video_config = Some(config);
                self.publish_initialization();
            }
            MediaEventKind::AacSequenceHeader => {
                let Some(config) = parse_aac_config(event.payload()) else {
                    self.failed = true;
                    return;
                };
                if self
                    .audio_config
                    .as_ref()
                    .is_some_and(|current| current != &config)
                {
                    self.failed = true;
                    return;
                }
                self.audio_config = Some(config);
                self.publish_initialization();
            }
            MediaEventKind::VideoKeyframe => {
                if self.video_config.is_none() || self.audio_config.is_none() {
                    return;
                }
                let timestamp_ms = event.timestamp_ms();
                if self.should_cut(timestamp_ms) {
                    self.finish(false);
                    if self.failed {
                        return;
                    }
                }
                self.append_video(event, timestamp_ms, true);
            }
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {
                if self.video_config.is_some() {
                    self.append_video(event, event.timestamp_ms(), false);
                }
            }
            MediaEventKind::Audio => {
                if self.audio_config.is_some() {
                    self.append_audio(event, event.timestamp_ms());
                }
            }
            MediaEventKind::HevcSequenceHeader | MediaEventKind::Av1SequenceHeader => {
                self.failed = true;
            }
        }
    }

    pub(crate) fn finish(&mut self, end_list: bool) {
        if self.failed {
            return;
        }
        let Some(segment) = self.current.take() else {
            if end_list {
                self.write_manifest(true);
            }
            return;
        };
        let (Some(video_config), Some(audio_config)) =
            (self.video_config.as_ref(), self.audio_config.as_ref())
        else {
            if end_list {
                self.write_manifest(true);
            }
            return;
        };
        if segment.video.is_empty() || segment.audio.is_empty() {
            if end_list {
                self.write_manifest(true);
            }
            return;
        }
        let bytes = make_fragment(
            video_config,
            audio_config,
            &segment,
            u32::try_from(self.next_sequence).unwrap_or(u32::MAX),
        );
        if bytes.len() > self.config.max_segment_bytes {
            self.failed = true;
            return;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let duration_ms = segment_duration_ms(&segment, audio_config.sample_rate);
        let Some(prefix) = self.store.current_prefix(&self.key) else {
            self.failed = true;
            return;
        };
        let directory = self.config.media_directory(&prefix);
        let path = directory.join(
            self.config
                .segment_filename(sequence, segment.start_timestamp_ms),
        );
        if self
            .store
            .publish(&self.key, self.incarnation, &path, &bytes)
            .is_err()
        {
            self.failed = true;
            return;
        }
        self.published.push_back(PublishedSegment {
            sequence,
            start_timestamp_ms: segment.start_timestamp_ms,
            duration_ms,
            path,
        });
        self.trim_playlist();
        self.write_manifest(end_list);
    }

    fn should_cut(&self, timestamp_ms: u32) -> bool {
        self.current.as_ref().is_some_and(|segment| {
            self.config
                .segment_window()
                .should_cut(segment.start_timestamp_ms, timestamp_ms)
        })
    }

    fn append_video(&mut self, event: &MediaEvent, timestamp_ms: u32, keyframe: bool) {
        let Some((data, composition_time_ms)) = parse_video_sample(event.payload()) else {
            self.failed = true;
            return;
        };
        let segment = self.current.get_or_insert_with(|| DashSegmentBuilder {
            start_timestamp_ms: timestamp_ms,
            last_timestamp_ms: timestamp_ms,
            video: Vec::new(),
            audio: Vec::new(),
        });
        segment.last_timestamp_ms = segment.last_timestamp_ms.max(timestamp_ms);
        segment.video.push(DashVideoSample {
            timestamp_ms,
            composition_time_ms,
            keyframe,
            data,
        });
    }

    fn append_audio(&mut self, event: &MediaEvent, timestamp_ms: u32) {
        let Some(data) = parse_audio_sample(event.payload()) else {
            self.failed = true;
            return;
        };
        let Some(segment) = &mut self.current else {
            return;
        };
        segment.last_timestamp_ms = segment.last_timestamp_ms.max(timestamp_ms);
        segment.audio.push(DashAudioSample { timestamp_ms, data });
    }

    fn publish_initialization(&mut self) {
        if self.initialization_published || self.failed {
            return;
        }
        let (Some(video_config), Some(audio_config)) =
            (self.video_config.as_ref(), self.audio_config.as_ref())
        else {
            return;
        };
        let Some(prefix) = self.store.current_prefix(&self.key) else {
            self.failed = true;
            return;
        };
        let directory = self.config.media_directory(&prefix);
        let initialization = make_initialization(
            video_config,
            audio_config,
            self.video_width,
            self.video_height,
        );
        if self
            .store
            .publish(
                &self.key,
                self.incarnation,
                &directory.join("init.mp4"),
                &initialization,
            )
            .is_err()
        {
            self.failed = true;
            return;
        }
        self.initialization_published = true;
    }

    fn trim_playlist(&mut self) {
        let playlist_length_ms = duration_millis(self.config.playlist_length);
        while self.published.len() > 1
            && (sum_durations(&self.published) > playlist_length_ms
                || self.published.len() > MAX_DASH_SEGMENTS)
        {
            let Some(segment) = self.published.pop_front() else {
                break;
            };
            if self.config.cleanup {
                let _ = self
                    .store
                    .remove(&self.key, self.incarnation, &segment.path);
            }
        }
    }

    fn write_manifest(&mut self, end_list: bool) {
        if self.failed || self.published.is_empty() {
            return;
        }
        let (Some(video_config), Some(audio_config)) =
            (self.video_config.as_ref(), self.audio_config.as_ref())
        else {
            return;
        };
        let Some(prefix) = self.store.current_prefix(&self.key) else {
            self.failed = true;
            return;
        };
        let directory = self.config.media_directory(&prefix);
        let manifest = render_mpd(
            &self.published,
            video_config,
            audio_config,
            self.video_width,
            self.video_height,
            self.config.playlist_length,
            end_list,
        );
        if manifest.len() > self.config.max_segment_bytes
            || self
                .store
                .publish(
                    &self.key,
                    self.incarnation,
                    &directory.join("manifest.mpd"),
                    manifest.as_bytes(),
                )
                .is_err()
        {
            self.failed = true;
        }
    }
}

#[cfg(test)]
mod tests;
