#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct HlsSegmenter {
    key: StreamKey,
    incarnation: PublisherIncarnation,
    store: Arc<MediaStore>,
    config: Arc<HlsOutputConfig>,
    variants: Vec<HlsVariant>,
    pub(super) video_config: Option<AvcConfig>,
    pub(super) audio_config: Option<AacConfig>,
    pub(super) current: Option<SegmentBuilder>,
    pub(super) published: VecDeque<PublishedSegment>,
    pub(super) next_sequence: u64,
    current_key: Option<KeyMaterial>,
    pub(super) failed: bool,
}

impl Drop for HlsSegmenter {
    fn drop(&mut self) {
        self.store.close(&self.key, self.incarnation);
    }
}

pub(super) struct SegmentBuilder {
    start_timestamp_ms: u32,
    last_timestamp_ms: u32,
    muxer: TsMuxer,
    has_video: bool,
    has_audio: bool,
    too_large: bool,
}

pub(super) struct PublishedSegment {
    sequence: u64,
    duration_ms: u64,
    paths: Vec<PathBuf>,
    key_paths: Vec<Option<PathBuf>>,
}

#[derive(Clone, Copy)]
struct KeyMaterial {
    id: u64,
    key: [u8; 16],
}

#[derive(Clone)]
pub(super) struct AvcConfig {
    pub(super) nal_length_size: usize,
    pub(super) sps: Vec<Vec<u8>>,
    pub(super) pps: Vec<Vec<u8>>,
}

#[derive(Clone, Copy)]
pub(super) struct AacConfig {
    pub(super) profile: u8,
    pub(super) sample_rate_index: u8,
    pub(super) channel_configuration: u8,
}

impl HlsSegmenter {
    pub(super) fn new(
        key: StreamKey,
        incarnation: PublisherIncarnation,
        store: Arc<MediaStore>,
        config: Arc<HlsOutputConfig>,
    ) -> Self {
        Self {
            key,
            incarnation,
            store,
            variants: config.variants(),
            config,
            video_config: None,
            audio_config: None,
            current: None,
            published: VecDeque::new(),
            next_sequence: 0,
            current_key: None,
            failed: false,
        }
    }

    pub(super) fn accept(&mut self, event: &MediaEvent, observed_at_unix_ms: u64) {
        if self.failed {
            return;
        }
        match event.kind() {
            MediaEventKind::AacSequenceHeader => {
                if let Some(config) = parse_aac_config(event.payload()) {
                    self.audio_config = Some(config);
                }
            }
            MediaEventKind::AvcSequenceHeader => {
                if let Some(config) = parse_avc_config(event.payload()) {
                    self.video_config = Some(config);
                    if self.current.is_some() {
                        self.finish(false);
                    }
                }
            }
            MediaEventKind::VideoKeyframe => {
                let Some(video_config) = self.video_config.clone() else {
                    return;
                };
                let Some(audio_config) = self.audio_config else {
                    return;
                };
                let timestamp_ms = event.timestamp_ms();
                if self.should_cut(timestamp_ms) {
                    self.finish(false);
                }
                if self.current.is_none() {
                    let mut muxer = TsMuxer::new(video_config, audio_config);
                    if muxer
                        .write_video(event, timestamp_ms, timestamp_ms, true)
                        .is_err()
                    {
                        return;
                    }
                    self.current = Some(SegmentBuilder {
                        start_timestamp_ms: timestamp_ms,
                        last_timestamp_ms: timestamp_ms,
                        muxer,
                        has_video: true,
                        has_audio: false,
                        too_large: false,
                    });
                } else {
                    self.append_video(event, timestamp_ms, true);
                }
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
            MediaEventKind::Metadata
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {}
        }
        let _ = observed_at_unix_ms;
    }

    fn should_cut(&self, timestamp_ms: u32) -> bool {
        self.current.as_ref().is_some_and(|segment| {
            self.config
                .segment_window()
                .should_cut(segment.start_timestamp_ms, timestamp_ms)
        })
    }

    fn append_video(&mut self, event: &MediaEvent, timestamp_ms: u32, keyframe: bool) {
        let Some(segment) = &mut self.current else {
            return;
        };
        if segment
            .muxer
            .write_video(event, timestamp_ms, segment.start_timestamp_ms, keyframe)
            .is_err()
        {
            segment.too_large = true;
        }
        segment.last_timestamp_ms = segment.last_timestamp_ms.max(timestamp_ms);
        segment.has_video = true;
        if segment.muxer.len() > self.config.max_segment_bytes {
            segment.too_large = true;
        }
    }

    fn append_audio(&mut self, event: &MediaEvent, timestamp_ms: u32) {
        let Some(segment) = &mut self.current else {
            return;
        };
        if segment
            .muxer
            .write_audio(event, timestamp_ms, segment.start_timestamp_ms)
            .is_err()
        {
            segment.too_large = true;
        }
        segment.last_timestamp_ms = segment.last_timestamp_ms.max(timestamp_ms);
        segment.has_audio = true;
        if segment.muxer.len() > self.config.max_segment_bytes {
            segment.too_large = true;
        }
    }

    pub(super) fn finish(&mut self, end_list: bool) {
        let Some(segment) = self.current.take() else {
            if end_list && !self.published.is_empty() {
                self.write_playlists(true);
            }
            return;
        };
        if segment.too_large || !segment.has_video || !segment.has_audio {
            if end_list && !self.published.is_empty() {
                self.write_playlists(true);
            }
            return;
        }
        let bytes = segment.muxer.finish();
        if bytes.len() > self.config.max_segment_bytes {
            if end_list && !self.published.is_empty() {
                self.write_playlists(true);
            }
            return;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let duration_ms = u64::from(
            segment
                .last_timestamp_ms
                .saturating_sub(segment.start_timestamp_ms),
        )
        .max(1);
        let Ok(key) = self.key_for(sequence) else {
            self.failed = true;
            return;
        };
        let Some(prefix) = self.store.current_prefix(&self.key) else {
            return;
        };
        let mut paths = Vec::with_capacity(self.variants.len());
        let mut key_paths = Vec::with_capacity(self.variants.len());
        for variant in &self.variants {
            let directory = self.config.variant_directory(&prefix, variant);
            let filename =
                fragment_filename(self.config.naming, sequence, segment.start_timestamp_ms);
            let path = directory.join(&filename);
            let encrypted = match key.as_ref() {
                Some(key) => encrypt_segment(&key.key, sequence, &bytes),
                None => bytes.clone(),
            };
            if self
                .store
                .publish(&self.key, self.incarnation, &path, &encrypted)
                .is_err()
            {
                self.failed = true;
                return;
            }
            let key_path = if let Some(key) = key.as_ref() {
                let key_path = directory.join(format!("key-{}.bin", key.id));
                if self
                    .store
                    .publish(&self.key, self.incarnation, &key_path, &key.key)
                    .is_err()
                {
                    self.failed = true;
                    return;
                }
                Some(key_path)
            } else {
                None
            };
            paths.push(path);
            key_paths.push(key_path);
        }
        self.published.push_back(PublishedSegment {
            sequence,
            duration_ms,
            paths,
            key_paths,
        });
        self.trim_playlist();
        self.write_playlists(end_list);
    }

    fn key_for(&mut self, sequence: u64) -> Result<Option<KeyMaterial>, MediaStoreError> {
        let Some(config) = &self.config.keys else {
            return Ok(None);
        };
        let rotation_segments = config.rotation_segments;
        let rotate = rotation_segments == 0
            || sequence == 0
            || sequence
                .is_multiple_of(u64::try_from(rotation_segments).expect("bounded key rotation"));
        if rotate || self.current_key.is_none() {
            let mut key = [0_u8; 16];
            rand_bytes(&mut key).map_err(|_| {
                MediaStoreError::Publish(std::io::Error::other("media key generation failed"))
            })?;
            self.current_key = Some(KeyMaterial { id: sequence, key });
        }
        Ok(self.current_key)
    }

    fn trim_playlist(&mut self) {
        let playlist_length_ms =
            u64::try_from(self.config.playlist_length.as_millis()).unwrap_or(u64::MAX);
        while self.published.len() > 1
            && (self
                .published
                .iter()
                .map(|segment| segment.duration_ms)
                .sum::<u64>()
                > playlist_length_ms
                || self.published.len() > MAX_PLAYLIST_SEGMENTS)
        {
            let Some(segment) = self.published.pop_front() else {
                break;
            };
            if self.config.cleanup {
                for path in segment.paths {
                    let _ = self.store.remove(&self.key, self.incarnation, &path);
                }
                for path in segment.key_paths.into_iter().flatten() {
                    if !self.published.iter().any(|candidate| {
                        candidate
                            .key_paths
                            .iter()
                            .flatten()
                            .any(|other| other == &path)
                    }) {
                        let _ = self.store.remove(&self.key, self.incarnation, &path);
                    }
                }
            }
        }
    }

    fn write_playlists(&mut self, end_list: bool) {
        let Some(prefix) = self.store.current_prefix(&self.key) else {
            return;
        };
        for (index, variant) in self.variants.iter().enumerate() {
            let directory = self.config.variant_directory(&prefix, variant);
            let key_url_prefix = self
                .config
                .keys
                .as_ref()
                .map_or("", |keys| keys.url_prefix.as_str());
            let playlist =
                render_variant_playlist(&self.published, index, key_url_prefix, end_list);
            if self
                .store
                .publish(
                    &self.key,
                    self.incarnation,
                    &directory.join("index.m3u8"),
                    playlist.as_bytes(),
                )
                .is_err()
            {
                self.failed = true;
                return;
            }
        }
        if self.variants.len() > 1 {
            let master = render_master_playlist(&self.config, &self.variants);
            if self
                .store
                .publish(
                    &self.key,
                    self.incarnation,
                    &prefix.join("master.m3u8"),
                    master.as_bytes(),
                )
                .is_err()
            {
                self.failed = true;
            }
        }
    }
}

fn fragment_filename(naming: HlsFragmentNaming, sequence: u64, timestamp_ms: u32) -> String {
    match naming {
        HlsFragmentNaming::Sequential => format!("seg-{sequence}.ts"),
        HlsFragmentNaming::Timestamp => format!("seg-{timestamp_ms}.ts"),
        HlsFragmentNaming::System => format!("seg-{}-{sequence}.ts", unix_time_ms()),
    }
}

fn render_master_playlist(config: &HlsOutputConfig, variants: &[HlsVariant]) -> String {
    let mut output = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    for variant in variants {
        output.push_str("#EXT-X-STREAM-INF:BANDWIDTH=");
        output.push_str(&variant.bandwidth.to_string());
        if let Some(codecs) = &variant.codecs {
            output.push_str(",CODECS=\"");
            output.push_str(codecs);
            output.push('"');
        }
        if let (Some(width), Some(height)) = (variant.width, variant.height) {
            let _ = write!(output, ",RESOLUTION={width}x{height}");
        }
        output.push('\n');
        if config.nested || variants.len() > 1 {
            output.push_str(&variant.name);
            output.push('/');
        }
        output.push_str("index.m3u8\n");
    }
    output
}

fn render_variant_playlist(
    segments: &VecDeque<PublishedSegment>,
    variant_index: usize,
    key_url_prefix: &str,
    end_list: bool,
) -> String {
    let target_duration = segments
        .iter()
        .map(|segment| segment.duration_ms.saturating_add(999) / 1_000)
        .max()
        .unwrap_or(1);
    let media_sequence = segments.front().map_or(0, |segment| segment.sequence);
    let mut output = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n#EXT-X-INDEPENDENT-SEGMENTS\n"
    );
    let mut previous_key = None;
    for segment in segments {
        let key_path = segment.key_paths[variant_index].as_ref();
        if key_path.map(PathBuf::as_path) != previous_key {
            if let Some(path) = key_path {
                output.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"");
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    output.push_str(key_url_prefix);
                    output.push_str(name);
                }
                output.push_str("\",IV=0x");
                let _ = write!(output, "{:032x}", segment.sequence);
                output.push('\n');
            }
            previous_key = key_path.map(PathBuf::as_path);
        }
        let path = &segment.paths[variant_index];
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            let _ = writeln!(
                output,
                "#EXTINF:{}.{:03},\n{name}",
                segment.duration_ms / 1_000,
                segment.duration_ms % 1_000
            );
        }
    }
    if end_list {
        output.push_str("#EXT-X-ENDLIST\n");
    }
    output
}

fn encrypt_segment(key: &[u8; 16], sequence: u64, bytes: &[u8]) -> Vec<u8> {
    let mut iv = [0_u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    symm::encrypt(symm::Cipher::aes_128_cbc(), key, Some(&iv), bytes)
        .expect("AES-128-CBC accepts a 16-byte HLS key and IV")
}

pub(super) fn parse_aac_config(payload: &[u8]) -> Option<AacConfig> {
    let parsed = parse_aac_configuration(payload)?;
    if !(1..=4).contains(&parsed.audio_object_type)
        || parsed.sample_rate_index == 15
        || parsed.sample_rate_index == 0
        || parsed.channel_configuration == 0
        || parsed.channel_configuration > 6
    {
        return None;
    }
    Some(AacConfig {
        profile: parsed.audio_object_type.saturating_sub(1),
        sample_rate_index: parsed.sample_rate_index,
        channel_configuration: parsed.channel_configuration,
    })
}

pub(super) fn parse_avc_config(payload: &[u8]) -> Option<AvcConfig> {
    let parsed = parse_avc_configuration(payload)?;
    if parsed.sps.is_empty() || parsed.pps.is_empty() {
        return None;
    }
    Some(AvcConfig {
        nal_length_size: parsed.nal_length_size,
        sps: parsed.sps.into_iter().map(Vec::from).collect(),
        pps: parsed.pps.into_iter().map(Vec::from).collect(),
    })
}
