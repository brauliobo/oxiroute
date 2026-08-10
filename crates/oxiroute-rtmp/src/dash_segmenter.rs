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
    media_parser::{parse_aac_configuration, parse_avc_configuration},
    media_storage::{MediaStore, MediaStoreError},
    segment_window::SegmentWindowConfig,
};

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
                format!("seg-{}-{sequence}.m4s", system_time_ms())
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

fn load_manifest(
    store: &MediaStore,
    directory: &Path,
) -> Result<(VecDeque<PublishedSegment>, u64), MediaStoreError> {
    let manifest_path = directory.join("manifest.mpd");
    let bytes = match store.read_relative(&manifest_path, store.limits().max_file_bytes) {
        Ok(bytes) => bytes,
        Err(MediaStoreError::NotFound) => return Ok((VecDeque::new(), 0)),
        Err(error) => return Err(error),
    };
    let text = str::from_utf8(&bytes).map_err(|_| MediaStoreError::ManifestMalformed)?;
    if !text.starts_with("<?xml")
        || !text.contains("<MPD ")
        || !text.contains("<SegmentList ")
        || !text.contains("</SegmentList>")
        || !text.contains("</MPD>")
    {
        return Err(MediaStoreError::ManifestMalformed);
    }
    let mut published = VecDeque::new();
    let mut cursor = 0;
    while let Some(start) = text[cursor..].find("<SegmentURL ") {
        let start = cursor + start;
        let end = text[start..]
            .find("/>")
            .map(|offset| start + offset)
            .ok_or(MediaStoreError::ManifestMalformed)?;
        let tag = &text[start..end];
        let media = xml_attribute(tag, "media").ok_or(MediaStoreError::ManifestMalformed)?;
        let duration_ms = xml_attribute(tag, "d")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|duration| *duration > 0)
            .ok_or(MediaStoreError::ManifestMalformed)?;
        let start_timestamp_ms = xml_attribute(tag, "t")
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(MediaStoreError::ManifestMalformed)?;
        if !valid_segment_filename(media) {
            return Err(MediaStoreError::ManifestMalformed);
        }
        let path = directory.join(media);
        store
            .read_relative(&path, store.limits().max_file_bytes)
            .map_err(|_| MediaStoreError::ManifestMalformed)?;
        let sequence = sequence_from_filename(media).ok_or(MediaStoreError::ManifestMalformed)?;
        if published
            .iter()
            .any(|segment: &PublishedSegment| segment.sequence == sequence)
        {
            return Err(MediaStoreError::ManifestMalformed);
        }
        published.push_back(PublishedSegment {
            sequence,
            start_timestamp_ms,
            duration_ms,
            path,
        });
        cursor = end + 2;
    }
    if published.is_empty() {
        return Err(MediaStoreError::ManifestMalformed);
    }
    let next_sequence = published
        .iter()
        .map(|segment| segment.sequence)
        .max()
        .and_then(|sequence| sequence.checked_add(1))
        .unwrap_or(0);
    Ok((published, next_sequence))
}

fn render_mpd(
    segments: &VecDeque<PublishedSegment>,
    video: &AvcConfig,
    audio: &AacConfig,
    width: u16,
    height: u16,
    playlist_length: Duration,
    end_list: bool,
) -> String {
    let total_duration_ms = segments
        .back()
        .map_or(0, |segment| {
            u64::from(segment.start_timestamp_ms).saturating_add(segment.duration_ms)
        })
        .saturating_sub(u64::from(
            segments
                .front()
                .map_or(0, |segment| segment.start_timestamp_ms),
        ));
    let mut output = String::with_capacity(2_048 + segments.len() * 128);
    output.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        output,
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"{}\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" minBufferTime=\"PT1S\" mediaPresentationDuration=\"{}\" timeShiftBufferDepth=\"{}\">",
        if end_list { "static" } else { "dynamic" },
        iso_duration(total_duration_ms),
        iso_duration(duration_millis(playlist_length)),
    );
    output.push_str("<Period id=\"0\" start=\"PT0S\">\n");
    let _ = writeln!(
        output,
        "<AdaptationSet id=\"0\" contentType=\"video\" mimeType=\"video/mp4\" codecs=\"{},mp4a.40.2\">",
        video.codec_string()
    );
    let _ = writeln!(
        output,
        "<Representation id=\"avc-aac\" bandwidth=\"1000000\"{}{}>",
        if width > 0 {
            format!(" width=\"{width}\"")
        } else {
            String::new()
        },
        if height > 0 {
            format!(" height=\"{height}\"")
        } else {
            String::new()
        },
    );
    let _ = writeln!(
        output,
        "<SegmentList timescale=\"1000\" presentationTimeOffset=\"0\"><Initialization sourceURL=\"init.mp4\"/>"
    );
    for segment in segments {
        let filename = segment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("invalid.m4s");
        let _ = writeln!(
            output,
            "<SegmentURL media=\"{filename}\" t=\"{}\" d=\"{}\"/>",
            segment.start_timestamp_ms, segment.duration_ms
        );
    }
    output.push_str("</SegmentList></Representation></AdaptationSet>\n");
    let _ = writeln!(
        output,
        "<!-- AAC sample rate {} Hz, {} channels, object type {} -->",
        audio.sample_rate, audio.channel_configuration, audio.audio_object_type
    );
    output.push_str("</Period></MPD>\n");
    output
}

fn make_initialization(video: &AvcConfig, audio: &AacConfig, width: u16, height: u16) -> Vec<u8> {
    let mut output = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso6");
    ftyp.extend_from_slice(&1_u32.to_be_bytes());
    ftyp.extend_from_slice(b"iso6");
    ftyp.extend_from_slice(b"dash");
    ftyp.extend_from_slice(b"mp41");
    append_box(&mut output, *b"ftyp", &ftyp);
    let mut moov = Vec::new();
    append_full_box(&mut moov, *b"mvhd", 0, &movie_header_payload());
    append_box(&mut moov, *b"trak", &video_track(video, width, height));
    append_box(&mut moov, *b"trak", &audio_track(audio));
    let mut mvex = Vec::new();
    append_full_box(&mut mvex, *b"trex", 0, &trex_payload(1));
    append_full_box(&mut mvex, *b"trex", 0, &trex_payload(2));
    append_box(&mut moov, *b"mvex", &mvex);
    append_box(&mut output, *b"moov", &moov);
    output
}

fn make_fragment(
    video: &AvcConfig,
    audio: &AacConfig,
    segment: &DashSegmentBuilder,
    sequence: u32,
) -> Vec<u8> {
    let video_bytes = segment
        .video
        .iter()
        .map(|sample| sample.data.len())
        .sum::<usize>();
    let audio_bytes = segment
        .audio
        .iter()
        .map(|sample| sample.data.len())
        .sum::<usize>();
    let empty_video = build_moof(sequence, video, audio, segment, 0, 0);
    let video_offset = i32::try_from(empty_video.len().saturating_add(8)).unwrap_or(i32::MAX);
    let audio_offset = video_offset.saturating_add(i32::try_from(video_bytes).unwrap_or(i32::MAX));
    let moof = build_moof(sequence, video, audio, segment, video_offset, audio_offset);
    let mut output = moof;
    let mut mdat = Vec::with_capacity(video_bytes.saturating_add(audio_bytes));
    for sample in &segment.video {
        mdat.extend_from_slice(&sample.data);
    }
    for sample in &segment.audio {
        mdat.extend_from_slice(&sample.data);
    }
    append_box(&mut output, *b"mdat", &mdat);
    output
}

fn build_moof(
    sequence: u32,
    _video: &AvcConfig,
    audio: &AacConfig,
    segment: &DashSegmentBuilder,
    video_offset: i32,
    audio_offset: i32,
) -> Vec<u8> {
    let mut moof = Vec::new();
    let mut mfhd = Vec::new();
    mfhd.extend_from_slice(&sequence.to_be_bytes());
    append_full_box(&mut moof, *b"mfhd", 0, &mfhd);
    append_box(&mut moof, *b"traf", &video_traf(segment, video_offset));
    append_box(
        &mut moof,
        *b"traf",
        &audio_traf(segment, audio, audio_offset),
    );
    let mut output = Vec::with_capacity(moof.len() + 8);
    append_box(&mut output, *b"moof", &moof);
    output
}

fn video_traf(segment: &DashSegmentBuilder, data_offset: i32) -> Vec<u8> {
    let mut traf = Vec::new();
    append_full_box(&mut traf, *b"tfhd", 0x0002_0000, &1_u32.to_be_bytes());
    let base_decode_time = segment
        .video
        .first()
        .map_or(0, |sample| u64::from(sample.timestamp_ms) * 90);
    append_full_box(
        &mut traf,
        *b"tfdt",
        0x0100_0000,
        &base_decode_time.to_be_bytes(),
    );
    let mut trun = Vec::new();
    trun.extend_from_slice(
        &u32::try_from(segment.video.len())
            .expect("DASH video sample count is bounded")
            .to_be_bytes(),
    );
    trun.extend_from_slice(&data_offset.to_be_bytes());
    for (index, sample) in segment.video.iter().enumerate() {
        trun.extend_from_slice(&video_sample_duration(&segment.video, index).to_be_bytes());
        trun.extend_from_slice(
            &u32::try_from(sample.data.len())
                .expect("DASH video sample size is bounded")
                .to_be_bytes(),
        );
        let flags: u32 = if sample.keyframe {
            0x0200_0000
        } else {
            0x0101_0000
        };
        trun.extend_from_slice(&flags.to_be_bytes());
        let composition_time = i32::try_from(
            i64::from(sample.composition_time_ms) * i64::from(VIDEO_TIMESCALE) / 1_000,
        )
        .unwrap_or_else(|_| {
            if sample.composition_time_ms.is_negative() {
                i32::MIN
            } else {
                i32::MAX
            }
        });
        trun.extend_from_slice(&composition_time.to_be_bytes());
    }
    append_full_box(&mut traf, *b"trun", 0x0100_0f01, &trun);
    traf
}

fn audio_traf(segment: &DashSegmentBuilder, audio: &AacConfig, data_offset: i32) -> Vec<u8> {
    let mut traf = Vec::new();
    append_full_box(&mut traf, *b"tfhd", 0x0002_0000, &2_u32.to_be_bytes());
    let base_decode_time = segment.audio.first().map_or(0, |sample| {
        u64::from(sample.timestamp_ms).saturating_mul(u64::from(audio.sample_rate)) / 1_000
    });
    append_full_box(
        &mut traf,
        *b"tfdt",
        0x0100_0000,
        &base_decode_time.to_be_bytes(),
    );
    let mut trun = Vec::new();
    trun.extend_from_slice(
        &u32::try_from(segment.audio.len())
            .expect("DASH audio sample count is bounded")
            .to_be_bytes(),
    );
    trun.extend_from_slice(&data_offset.to_be_bytes());
    for sample in &segment.audio {
        trun.extend_from_slice(&AAC_SAMPLES_PER_FRAME.to_be_bytes());
        trun.extend_from_slice(
            &u32::try_from(sample.data.len())
                .expect("DASH audio sample size is bounded")
                .to_be_bytes(),
        );
    }
    append_full_box(&mut traf, *b"trun", 0x0000_0301, &trun);
    traf
}

fn video_sample_duration(samples: &[DashVideoSample], index: usize) -> u32 {
    samples
        .get(index + 1)
        .map_or(DEFAULT_VIDEO_SAMPLE_DURATION, |next| {
            let current = samples[index].timestamp_ms;
            let elapsed = next.timestamp_ms.saturating_sub(current);
            (u64::from(elapsed) * u64::from(VIDEO_TIMESCALE) / 1_000)
                .try_into()
                .unwrap_or(u32::MAX)
                .max(1)
        })
}

fn movie_header_payload() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0x0100_u16.to_be_bytes());
    payload.extend_from_slice(&[0; 10]);
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x4000_0000_u32.to_be_bytes());
    payload.extend_from_slice(&[0; 24]);
    payload.extend_from_slice(&3_u32.to_be_bytes());
    payload
}

fn trex_payload(track_id: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&track_id.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload
}

fn video_track(config: &AvcConfig, width: u16, height: u16) -> Vec<u8> {
    let mut trak = Vec::new();
    append_full_box(
        &mut trak,
        *b"tkhd",
        0x0000_0007,
        &track_header_payload(1, 0, width, height),
    );
    let mut mdia = Vec::new();
    append_full_box(
        &mut mdia,
        *b"mdhd",
        0,
        &media_header_payload(VIDEO_TIMESCALE),
    );
    append_full_box(
        &mut mdia,
        *b"hdlr",
        0,
        &handler_payload(*b"vide", "VideoHandler"),
    );
    let mut minf = Vec::new();
    append_full_box(&mut minf, *b"vmhd", 1, &[0; 8]);
    append_box(&mut minf, *b"dinf", &data_information());
    append_box(
        &mut minf,
        *b"stbl",
        &video_sample_table(config, width, height),
    );
    append_box(&mut mdia, *b"minf", &minf);
    append_box(&mut trak, *b"mdia", &mdia);
    trak
}

fn audio_track(config: &AacConfig) -> Vec<u8> {
    let mut trak = Vec::new();
    append_full_box(
        &mut trak,
        *b"tkhd",
        0x0000_0007,
        &track_header_payload(2, 0x0100, 0, 0),
    );
    let mut mdia = Vec::new();
    append_full_box(
        &mut mdia,
        *b"mdhd",
        0,
        &media_header_payload(config.sample_rate),
    );
    append_full_box(
        &mut mdia,
        *b"hdlr",
        0,
        &handler_payload(*b"soun", "SoundHandler"),
    );
    let mut minf = Vec::new();
    append_full_box(&mut minf, *b"smhd", 0, &[0; 4]);
    append_box(&mut minf, *b"dinf", &data_information());
    append_box(&mut minf, *b"stbl", &audio_sample_table(config));
    append_box(&mut mdia, *b"minf", &minf);
    append_box(&mut trak, *b"mdia", &mdia);
    trak
}

fn track_header_payload(track_id: u32, volume: u16, width: u16, height: u16) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&track_id.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&[0; 8]);
    payload.extend_from_slice(&0_i16.to_be_bytes());
    payload.extend_from_slice(&0_i16.to_be_bytes());
    payload.extend_from_slice(&volume.to_be_bytes());
    payload.extend_from_slice(&0_u16.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x4000_0000_u32.to_be_bytes());
    payload.extend_from_slice(&[0; 24]);
    payload.extend_from_slice(&(u32::from(width) << 16).to_be_bytes());
    payload.extend_from_slice(&(u32::from(height) << 16).to_be_bytes());
    payload
}

fn media_header_payload(timescale: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&timescale.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x55c4_u16.to_be_bytes());
    payload.extend_from_slice(&0_u16.to_be_bytes());
    payload
}

fn handler_payload(handler: [u8; 4], name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&handler);
    payload.extend_from_slice(&[0; 12]);
    payload.extend_from_slice(name.as_bytes());
    payload.push(0);
    payload
}

fn data_information() -> Vec<u8> {
    let mut dref = Vec::new();
    dref.extend_from_slice(&1_u32.to_be_bytes());
    append_full_box(&mut dref, *b"url ", 1, &[]);
    let mut dinf = Vec::new();
    append_full_box(&mut dinf, *b"dref", 0, &dref);
    dinf
}

fn video_sample_table(config: &AvcConfig, width: u16, height: u16) -> Vec<u8> {
    let mut avc1 = Vec::new();
    avc1.extend_from_slice(&[0; 6]);
    avc1.extend_from_slice(&1_u16.to_be_bytes());
    avc1.extend_from_slice(&0_u16.to_be_bytes());
    avc1.extend_from_slice(&0_u16.to_be_bytes());
    avc1.extend_from_slice(&[0; 12]);
    avc1.extend_from_slice(&width.to_be_bytes());
    avc1.extend_from_slice(&height.to_be_bytes());
    avc1.extend_from_slice(&0x0048_0000_u32.to_be_bytes());
    avc1.extend_from_slice(&0x0048_0000_u32.to_be_bytes());
    avc1.extend_from_slice(&0_u32.to_be_bytes());
    avc1.extend_from_slice(&1_u16.to_be_bytes());
    avc1.extend_from_slice(&[0; 32]);
    avc1.extend_from_slice(&0x0018_u16.to_be_bytes());
    avc1.extend_from_slice(&0xffff_u16.to_be_bytes());
    append_box(&mut avc1, *b"avcC", &config.configuration_record);
    let mut stsd_payload = 1_u32.to_be_bytes().to_vec();
    append_box(&mut stsd_payload, *b"avc1", &avc1);
    let mut stbl = Vec::new();
    append_full_box(&mut stbl, *b"stsd", 0, &stsd_payload);
    append_full_box(&mut stbl, *b"stts", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsc", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsz", 0, &[0; 8]);
    append_full_box(&mut stbl, *b"stco", 0, &0_u32.to_be_bytes());
    stbl
}

fn audio_sample_table(config: &AacConfig) -> Vec<u8> {
    let mut mp4a = Vec::new();
    mp4a.extend_from_slice(&[0; 6]);
    mp4a.extend_from_slice(&1_u16.to_be_bytes());
    mp4a.extend_from_slice(&[0; 8]);
    mp4a.extend_from_slice(&u16::from(config.channel_configuration).to_be_bytes());
    mp4a.extend_from_slice(&16_u16.to_be_bytes());
    mp4a.extend_from_slice(&0_u16.to_be_bytes());
    mp4a.extend_from_slice(&0_u16.to_be_bytes());
    mp4a.extend_from_slice(&(config.sample_rate << 16).to_be_bytes());
    let decoder_config = descriptor(0x05, &config.audio_specific_config);
    let mut decoder = Vec::new();
    decoder.push(0x40);
    decoder.push(0x15);
    decoder.extend_from_slice(&[0; 3]);
    decoder.extend_from_slice(&[0; 4]);
    decoder.extend_from_slice(&[0; 4]);
    decoder.extend_from_slice(&decoder_config);
    let mut es = Vec::new();
    es.extend_from_slice(&1_u16.to_be_bytes());
    es.push(0);
    es.extend_from_slice(&descriptor(0x04, &decoder));
    es.extend_from_slice(&descriptor(0x06, &[2]));
    append_full_box(&mut mp4a, *b"esds", 0, &es);
    let mut stsd_payload = 1_u32.to_be_bytes().to_vec();
    append_box(&mut stsd_payload, *b"mp4a", &mp4a);
    let mut stbl = Vec::new();
    append_full_box(&mut stbl, *b"stsd", 0, &stsd_payload);
    append_full_box(&mut stbl, *b"stts", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsc", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsz", 0, &[0; 8]);
    append_full_box(&mut stbl, *b"stco", 0, &0_u32.to_be_bytes());
    stbl
}

fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(payload.len() + 2);
    output.push(tag);
    output.push(u8::try_from(payload.len()).expect("DASH descriptor is bounded"));
    output.extend_from_slice(payload);
    output
}

fn append_box(output: &mut Vec<u8>, kind: [u8; 4], payload: &[u8]) {
    let size = u32::try_from(payload.len().saturating_add(8)).expect("DASH box is bounded");
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(payload);
}

fn append_full_box(output: &mut Vec<u8>, kind: [u8; 4], version_flags: u32, payload: &[u8]) {
    let mut full_payload = Vec::with_capacity(payload.len() + 4);
    full_payload.extend_from_slice(&version_flags.to_be_bytes());
    full_payload.extend_from_slice(payload);
    append_box(output, kind, &full_payload);
}

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_media_configuration(payload: &[u8]) {
    let _ = parse_avc_config(payload);
    let _ = parse_aac_config(payload);
}

fn parse_avc_config(payload: &[u8]) -> Option<AvcConfig> {
    let parsed = parse_avc_configuration(payload)?;
    if parsed.nal_length_size != 4
        || parsed.sps.is_empty()
        || parsed.sps.len() > MAX_CODEC_PARAMETER_SETS
        || parsed.pps.is_empty()
        || parsed.pps.len() > MAX_CODEC_PARAMETER_SETS
        || !parsed.trailing.is_empty()
    {
        return None;
    }
    if parsed.sps.iter().any(|parameter_set| {
        parameter_set.is_empty()
            || parameter_set.len() > MAX_CODEC_PARAMETER_SET_BYTES
            || parameter_set[0] & 0x1f != 7
    }) || parsed.pps.iter().any(|parameter_set| {
        parameter_set.is_empty()
            || parameter_set.len() > MAX_CODEC_PARAMETER_SET_BYTES
            || parameter_set[0] & 0x1f != 8
    }) {
        return None;
    }
    Some(AvcConfig {
        profile: parsed.profile,
        compatibility: parsed.compatibility,
        level: parsed.level,
        configuration_record: parsed.configuration_record.to_vec(),
    })
}

fn parse_aac_config(payload: &[u8]) -> Option<AacConfig> {
    let parsed = parse_aac_configuration(payload)?;
    let sample_rate = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ]
    .get(usize::from(parsed.sample_rate_index))
    .copied()?;
    if parsed.audio_object_type != 2
        || parsed.channel_configuration == 0
        || parsed.channel_configuration > 6
    {
        return None;
    }
    let mut audio_specific_config = Vec::with_capacity(
        parsed
            .audio_specific_config
            .len()
            .saturating_add(parsed.trailing.len()),
    );
    audio_specific_config.extend_from_slice(parsed.audio_specific_config);
    audio_specific_config.extend_from_slice(parsed.trailing);
    Some(AacConfig {
        audio_object_type: parsed.audio_object_type,
        sample_rate,
        channel_configuration: parsed.channel_configuration,
        audio_specific_config,
    })
}

fn parse_video_sample(payload: &[u8]) -> Option<(Vec<u8>, i32)> {
    if payload.len() < 9 || payload[0] & 0x0f != 7 || payload[1] != 1 {
        return None;
    }
    let composition_time_ms = signed_24(&payload[2..5]);
    let mut cursor = 5;
    let mut data = Vec::with_capacity(payload.len() - 5);
    while cursor < payload.len() {
        let end = cursor.checked_add(4)?;
        let length = usize::try_from(u32::from_be_bytes([
            *payload.get(cursor)?,
            *payload.get(cursor + 1)?,
            *payload.get(cursor + 2)?,
            *payload.get(cursor + 3)?,
        ]))
        .ok()?;
        cursor = end;
        if length == 0 {
            return None;
        }
        let end = cursor.checked_add(length)?;
        if payload.get(cursor..end)?.is_empty() {
            return None;
        }
        data.extend_from_slice(&payload[cursor - 4..end]);
        cursor = end;
    }
    (!data.is_empty()).then_some((data, composition_time_ms))
}

fn parse_audio_sample(payload: &[u8]) -> Option<Vec<u8>> {
    (payload.len() > 2 && payload[0] >> 4 == 10 && payload[1] == 1)
        .then(|| payload[2..].to_vec())
        .filter(|data| !data.is_empty())
}

fn signed_24(bytes: &[u8]) -> i32 {
    let value = (i32::from(bytes[0]) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[2]);
    if value & 0x80_0000 != 0 {
        value | !0x00ff_ffff
    } else {
        value
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn duration_millis_for_audio(sample_count: usize, sample_rate: u32) -> u64 {
    u64::try_from(sample_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::from(AAC_SAMPLES_PER_FRAME))
        .saturating_mul(1_000)
        / u64::from(sample_rate.max(1))
}

fn segment_duration_ms(segment: &DashSegmentBuilder, sample_rate: u32) -> u64 {
    let audio_end = segment.audio.last().map_or(0, |sample| {
        u64::from(sample.timestamp_ms).saturating_add(duration_millis_for_audio(1, sample_rate))
    });
    u64::from(segment.last_timestamp_ms)
        .max(audio_end)
        .saturating_sub(u64::from(segment.start_timestamp_ms))
        .max(1)
}

fn sum_durations(segments: &VecDeque<PublishedSegment>) -> u64 {
    segments.iter().fold(0, |total, segment| {
        total.saturating_add(segment.duration_ms)
    })
}

fn iso_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let remainder = milliseconds % 1_000;
    if remainder == 0 {
        format!("PT{seconds}S")
    } else {
        let mut fraction = format!("{remainder:03}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        format!("PT{seconds}.{fraction}S")
    }
}

fn xml_attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=\"");
    let start = tag.find(&prefix)? + prefix.len();
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

fn valid_segment_filename(value: &str) -> bool {
    value.len() <= MAX_DASH_FILENAME_BYTES
        && value.is_ascii()
        && Path::new(value)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("m4s"))
        && value.starts_with("seg-")
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && sequence_from_filename(value).is_some()
}

fn sequence_from_filename(value: &str) -> Option<u64> {
    value.strip_suffix(".m4s")?.rsplit('-').next()?.parse().ok()
}

fn system_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::*;
    use crate::{LiveHub, LiveHubLimits, MediaStoreLimits};

    mod media_config_corpus {
        include!("../tests/corpus/media_configs.rs");
    }

    fn avc_sequence_header() -> Vec<u8> {
        vec![
            0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1, 0, 2,
            0x68, 0xce,
        ]
    }

    fn avc_frame(keyframe: bool, marker: u8) -> Vec<u8> {
        vec![
            if keyframe { 0x17 } else { 0x27 },
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            2,
            if keyframe { 0x65 } else { 0x41 },
            marker,
        ]
    }

    fn aac_sequence_header() -> Vec<u8> {
        vec![0xaf, 0, 0x12, 0x10]
    }

    fn aac_frame(marker: u8) -> Vec<u8> {
        vec![0xaf, 1, marker, 0x21, 0x10]
    }

    fn config(store: Arc<MediaStore>) -> Arc<DashOutputConfig> {
        Arc::new(DashOutputConfig {
            store,
            segment_duration: Duration::from_secs(1),
            max_segment_duration: Duration::from_secs(2),
            playlist_length: Duration::from_secs(4),
            naming: DashSegmentNaming::Sequential,
            nested: true,
            cleanup: true,
            max_segment_bytes: 1024 * 1024,
            max_queue_messages: 16,
        })
    }

    fn segmenter(root: &std::path::Path) -> (DashSegmenter, Arc<MediaStore>, StreamKey) {
        let store = Arc::new(
            MediaStore::open(
                root.join("dash"),
                MediaStoreLimits {
                    max_bytes: 4 * 1024 * 1024,
                    max_files: 64,
                    max_active_streams: 2,
                    max_file_bytes: 1024 * 1024,
                },
            )
            .expect("DASH store"),
        );
        let hub = LiveHub::new(LiveHubLimits::default());
        let key = StreamKey::new("live", "broadcast", "camera");
        let lease = hub.attach_publisher(key.clone()).expect("publisher");
        store
            .attach_continuing(&key, lease.incarnation())
            .expect("DASH media incarnation");
        let segmenter = DashSegmenter::new(
            key.clone(),
            lease.incarnation(),
            Arc::clone(&store),
            config(Arc::clone(&store)),
        )
        .expect("DASH segmenter");
        (segmenter, store, key)
    }

    #[test]
    fn characterizes_avc_configuration_acceptance() {
        for case in media_config_corpus::avc_cases() {
            assert_eq!(
                parse_avc_config(&case.payload).is_some(),
                case.accepted_by(media_config_corpus::Consumer::Dash),
                "{}",
                case.name,
            );
        }
    }

    #[test]
    fn characterizes_aac_configuration_acceptance() {
        for case in media_config_corpus::aac_cases() {
            assert_eq!(
                parse_aac_config(&case.payload).is_some(),
                case.accepted_by(media_config_corpus::Consumer::Dash),
                "{}",
                case.name,
            );
        }
    }

    #[test]
    fn invalid_configuration_permanently_fails_the_segmenter() {
        let root = tempdir().expect("DASH root");
        let (mut segmenter, store, key) = segmenter(root.path());
        let mut invalid = avc_sequence_header();
        invalid[5] = 2;
        segmenter.accept(&MediaEvent::video(0, Arc::<[u8]>::from(invalid)).expect("invalid event"));
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header()))
                .expect("valid AVC config"),
        );
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header()))
                .expect("valid AAC config"),
        );
        segmenter.finish(true);

        assert!(segmenter.failed);
        assert!(segmenter.video_config.is_none());
        assert!(segmenter.audio_config.is_none());
        let prefix = store.current_prefix(&key).expect("current prefix");
        assert!(
            store
                .read_relative(&prefix.join("dash/init.mp4"), 1024)
                .is_err()
        );
    }

    #[test]
    fn changed_avc_or_aac_configuration_permanently_fails_the_segmenter() {
        {
            let root = tempdir().expect("DASH AVC root");
            let (mut segmenter, _, _) = segmenter(root.path());
            segmenter.accept(
                &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header()))
                    .expect("AVC config"),
            );
            let mut changed_avc = avc_sequence_header();
            changed_avc[6] = 0x4d;
            segmenter.accept(
                &MediaEvent::video(0, Arc::<[u8]>::from(changed_avc)).expect("changed AVC config"),
            );
            assert!(segmenter.failed);
        }

        {
            let root = tempdir().expect("DASH AAC root");
            let (mut segmenter, _, _) = segmenter(root.path());
            segmenter.accept(
                &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header()))
                    .expect("AAC config"),
            );
            segmenter.accept(
                &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x11, 0x90].as_slice()))
                    .expect("changed AAC config"),
            );
            assert!(segmenter.failed);
        }
    }

    #[test]
    fn repeatedly_accepts_identical_configurations_but_retains_aac_trailing_bytes() {
        let root = tempdir().expect("DASH root");
        let (mut segmenter, _, _) = segmenter(root.path());
        for _ in 0..2 {
            segmenter.accept(
                &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header()))
                    .expect("identical AVC config"),
            );
            segmenter.accept(
                &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header()))
                    .expect("identical AAC config"),
            );
        }
        assert!(!segmenter.failed);
        assert!(segmenter.initialization_published);

        let mut trailing = aac_sequence_header();
        trailing.push(0xaa);
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from(trailing)).expect("AAC trailing bytes"),
        );
        assert!(segmenter.failed);
    }

    #[test]
    fn enhanced_avc_hevc_and_av1_headers_permanently_fail_the_segmenter() {
        for four_cc in [*b"avc1", *b"hvc1", *b"av01"] {
            let root = tempdir().expect("DASH root");
            let (mut segmenter, _, _) = segmenter(root.path());
            let mut payload = vec![0x90];
            payload.extend_from_slice(&four_cc);
            payload.push(1);
            segmenter.accept(
                &MediaEvent::video(0, Arc::<[u8]>::from(payload)).expect("enhanced header"),
            );
            assert!(segmenter.failed, "{}", four_cc.escape_ascii());
        }
    }

    #[test]
    fn writes_player_parseable_fragmented_mp4_and_bounded_mpd() {
        let root = tempdir().expect("DASH root");
        let (mut segmenter, store, key) = segmenter(root.path());
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header())).expect("AAC config"),
        );
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header())).expect("AVC config"),
        );
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(avc_frame(true, 1))).expect("keyframe"),
        );
        segmenter
            .accept(&MediaEvent::audio(0, Arc::<[u8]>::from(aac_frame(2))).expect("AAC frame"));
        segmenter.accept(
            &MediaEvent::video(1_000, Arc::<[u8]>::from(avc_frame(false, 3))).expect("interframe"),
        );
        segmenter.accept(
            &MediaEvent::video(2_000, Arc::<[u8]>::from(avc_frame(true, 4)))
                .expect("boundary keyframe"),
        );
        segmenter.finish(true);

        let prefix = store.current_prefix(&key).expect("current prefix");
        let directory = prefix.join("dash");
        let init = store
            .read_relative(&directory.join("init.mp4"), 1024 * 1024)
            .expect("initialization");
        let fragment = store
            .read_relative(&directory.join("seg-0.m4s"), 1024 * 1024)
            .expect("fragment");
        let manifest = store
            .read_relative(&directory.join("manifest.mpd"), 1024 * 1024)
            .expect("manifest");
        assert_eq!(&init[4..8], b"ftyp");
        assert!(contains_box(&init, *b"moov"));
        assert!(contains_box(&fragment, *b"moof"));
        assert!(contains_box(&fragment, *b"mdat"));
        let manifest = String::from_utf8(manifest).expect("MPD UTF-8");
        assert!(manifest.contains("type=\"static\""));
        assert!(manifest.contains("mimeType=\"video/mp4\""));
        assert!(manifest.contains("media=\"seg-0.m4s\""));
        assert!(!manifest.contains(".ts"));
    }

    #[test]
    fn rejects_unsupported_codec_forms_before_publishing_dash_files() {
        let root = tempdir().expect("DASH root");
        let (mut segmenter, store, key) = segmenter(root.path());
        let mut invalid = avc_sequence_header();
        invalid[9] = 0xfe;
        segmenter.accept(&MediaEvent::video(0, Arc::<[u8]>::from(invalid)).expect("event"));
        segmenter.finish(true);
        let prefix = store.current_prefix(&key).expect("current prefix");
        assert!(
            store
                .read_relative(&prefix.join("dash/manifest.mpd"), 1024)
                .is_err()
        );
    }

    #[test]
    fn parses_only_safe_segment_names() {
        assert!(valid_segment_filename("seg-42.m4s"));
        assert!(valid_segment_filename("seg-100-42.m4s"));
        assert!(!valid_segment_filename("../seg-42.m4s"));
        assert!(!valid_segment_filename("seg-42.ts"));
        assert!(!valid_segment_filename("seg-x.m4s"));
    }

    #[test]
    fn rejects_malformed_persisted_manifest() {
        let root = tempdir().expect("DASH root");
        let (segmenter, store, key) = segmenter(root.path());
        let prefix = store.current_prefix(&key).expect("current prefix");
        store
            .publish(
                &key,
                segmenter.incarnation,
                &prefix.join("dash/manifest.mpd"),
                b"not an MPD",
            )
            .expect("malformed manifest");
        let result = DashSegmenter::new(
            key,
            segmenter.incarnation,
            Arc::clone(&store),
            config(Arc::clone(&store)),
        );
        drop(segmenter);
        assert!(matches!(result, Err(MediaStoreError::ManifestMalformed)));
    }

    fn contains_box(bytes: &[u8], kind: [u8; 4]) -> bool {
        bytes.windows(4).any(|window| window == kind)
    }
}
