use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};

use openssl::{rand::rand_bytes, symm};

use crate::{
    MediaEvent, MediaEventKind, PublisherIncarnation, StreamKey,
    dash_segmenter::{DashOutputConfig, DashSegmenter},
    media_storage::{MediaStore, MediaStoreError},
};

const TS_PACKET_BYTES: usize = 188;
const VIDEO_PID: u16 = 0x0101;
const AUDIO_PID: u16 = 0x0102;
const PMT_PID: u16 = 0x1000;
const MAX_PLAYLIST_SEGMENTS: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HlsFragmentNaming {
    Sequential,
    Timestamp,
    System,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsVariant {
    pub name: String,
    pub bandwidth: u64,
    pub codecs: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsKeyConfig {
    pub rotation_segments: usize,
    pub url_prefix: String,
}

#[derive(Clone)]
pub struct HlsOutputConfig {
    pub store: Arc<MediaStore>,
    pub segment_duration: Duration,
    pub max_segment_duration: Duration,
    pub playlist_length: Duration,
    pub naming: HlsFragmentNaming,
    pub nested: bool,
    pub cleanup: bool,
    pub variants: Vec<HlsVariant>,
    pub keys: Option<HlsKeyConfig>,
    pub max_segment_bytes: usize,
    pub max_queue_messages: usize,
}

impl HlsOutputConfig {
    fn variants(&self) -> Vec<HlsVariant> {
        if self.variants.is_empty() {
            return vec![HlsVariant {
                name: "main".into(),
                bandwidth: 1_000_000,
                codecs: Some("avc1.42e01e,mp4a.40.2".into()),
                width: None,
                height: None,
            }];
        }
        self.variants.clone()
    }

    fn variant_directory(&self, prefix: &Path, variant: &HlsVariant) -> PathBuf {
        let variants = self.variants();
        if self.nested || variants.len() > 1 {
            prefix.join(&variant.name)
        } else {
            prefix.to_path_buf()
        }
    }
}

#[derive(Clone, Default)]
pub struct MediaApplication {
    hls: Option<Arc<HlsOutputConfig>>,
    dash: Option<Arc<DashOutputConfig>>,
}

impl MediaApplication {
    #[must_use]
    pub fn new(hls: Option<Arc<HlsOutputConfig>>) -> Self {
        Self { hls, dash: None }
    }

    #[must_use]
    pub fn hls(&self) -> Option<&Arc<HlsOutputConfig>> {
        self.hls.as_ref()
    }

    #[must_use]
    pub fn with_dash(mut self, dash: Option<Arc<DashOutputConfig>>) -> Self {
        self.dash = dash;
        self
    }

    #[must_use]
    pub fn dash(&self) -> Option<&Arc<DashOutputConfig>> {
        self.dash.as_ref()
    }

    pub(crate) fn attach(
        &self,
        key: &StreamKey,
        incarnation: PublisherIncarnation,
    ) -> Result<Option<MediaPublisher>, MediaOutputError> {
        if self.hls.is_none() && self.dash.is_none() {
            return Ok(None);
        }
        MediaPublisher::start(key, incarnation, self).map(Some)
    }
}

#[derive(Clone, Default)]
pub struct MediaCatalog {
    applications: Arc<BTreeMap<(String, String), Arc<MediaApplication>>>,
}

impl MediaCatalog {
    #[must_use]
    pub fn from_applications(
        applications: impl IntoIterator<Item = (String, String, Arc<MediaApplication>)>,
    ) -> Self {
        Self {
            applications: Arc::new(
                applications
                    .into_iter()
                    .map(|(service, application, media)| ((service, application), media))
                    .collect(),
            ),
        }
    }

    #[must_use]
    pub fn merge(catalogs: impl IntoIterator<Item = Arc<Self>>) -> Self {
        let mut applications = BTreeMap::new();
        for catalog in catalogs {
            applications.extend(
                catalog
                    .applications
                    .iter()
                    .map(|(key, value)| (key.clone(), Arc::clone(value))),
            );
        }
        Self {
            applications: Arc::new(applications),
        }
    }

    pub(crate) fn read(
        &self,
        service: &str,
        application: &str,
        stream: &str,
        object: &str,
    ) -> Result<MediaObject, MediaStoreError> {
        let media = self
            .applications
            .get(&(service.to_owned(), application.to_owned()))
            .ok_or(MediaStoreError::NotFound)?;
        let key = StreamKey::new(service, application, stream);
        if let Some(dash) = &media.dash {
            if let Some(prefix) = dash.store.current_prefix(&key) {
                match resolve_dash_public_path(dash, &prefix, object) {
                    Ok((path, content_type)) => {
                        let body = dash
                            .store
                            .read_relative(&path, dash.store.limits().max_file_bytes)?;
                        return Ok(MediaObject { body, content_type });
                    }
                    Err(MediaStoreError::InvalidPath) => {
                        return Err(MediaStoreError::InvalidPath);
                    }
                    Err(MediaStoreError::NotFound) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        if let Some(hls) = &media.hls {
            let prefix = hls
                .store
                .current_prefix(&key)
                .ok_or(MediaStoreError::NotFound)?;
            let (path, content_type) = resolve_public_path(hls, &prefix, object)?;
            let body = hls
                .store
                .read_relative(&path, hls.store.limits().max_file_bytes)?;
            return Ok(MediaObject { body, content_type });
        }
        Err(MediaStoreError::NotFound)
    }

    /// Reads one bounded public HLS or DASH object for an active publisher incarnation.
    ///
    /// # Errors
    ///
    /// Returns an error when the application, stream, or object is unavailable, invalid, or
    /// exceeds the configured file bound.
    pub fn read_object(
        &self,
        service: &str,
        application: &str,
        stream: &str,
        object: &str,
    ) -> Result<MediaObject, MediaStoreError> {
        self.read(service, application, stream, object)
    }
}

pub struct MediaObject {
    pub body: Vec<u8>,
    pub content_type: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaEnqueueResult {
    Queued,
    Dropped,
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaOutputError {
    #[error("media worker cannot be started")]
    WorkerSpawn(#[source] std::io::Error),
    #[error("media storage failed")]
    Storage(#[from] MediaStoreError),
}

pub struct MediaPublisher {
    sender: SyncSender<MediaCommand>,
    closed: Arc<AtomicBool>,
}

enum MediaCommand {
    Event {
        event: MediaEvent,
        observed_at_unix_ms: u64,
    },
    Close,
}

enum MediaWorker {
    Hls(HlsSegmenter),
    Dash(DashSegmenter),
}

impl MediaPublisher {
    fn start(
        key: &StreamKey,
        incarnation: PublisherIncarnation,
        config: &MediaApplication,
    ) -> Result<Self, MediaOutputError> {
        let hls = config.hls.clone();
        let dash = config.dash.clone();
        let dash_store = dash.as_ref().map(|output| Arc::clone(&output.store));
        let mut stores = Vec::<Arc<MediaStore>>::new();
        for store in hls
            .as_ref()
            .map(|output| Arc::clone(&output.store))
            .into_iter()
            .chain(dash_store.clone())
        {
            if stores.iter().any(|current| Arc::ptr_eq(current, &store)) {
                continue;
            }
            let preserve = dash_store
                .as_ref()
                .is_some_and(|dash_store| Arc::ptr_eq(dash_store, &store));
            let attached = if preserve {
                store.attach_continuing(key, incarnation)
            } else {
                store.attach(key, incarnation)
            };
            if let Err(error) = attached {
                for attached_store in &stores {
                    attached_store.close(key, incarnation);
                }
                return Err(MediaOutputError::Storage(error));
            }
            stores.push(store);
        }
        let queue_messages = hls
            .as_ref()
            .map_or(0, |output| output.max_queue_messages)
            .max(dash.as_ref().map_or(0, |output| output.max_queue_messages));
        let (sender, receiver) = mpsc::sync_channel(queue_messages.max(1));
        let closed = Arc::new(AtomicBool::new(false));
        let worker_key = key.clone();
        let worker_closed = Arc::clone(&closed);
        let mut workers = Vec::new();
        if let Some(hls) = hls {
            let store = Arc::clone(&hls.store);
            workers.push(MediaWorker::Hls(HlsSegmenter::new(
                worker_key.clone(),
                incarnation,
                store,
                hls,
            )));
        }
        if let Some(dash) = dash {
            let store = Arc::clone(&dash.store);
            match DashSegmenter::new(worker_key.clone(), incarnation, store, dash) {
                Ok(segmenter) => workers.push(MediaWorker::Dash(segmenter)),
                Err(error) => {
                    for store in stores {
                        store.close(key, incarnation);
                    }
                    return Err(MediaOutputError::Storage(error));
                }
            }
        }
        thread::Builder::new()
            .name("oxiroute-rtmp-media".into())
            .spawn(move || {
                run_worker(&mut workers, &receiver, &worker_closed);
            })
            .map_err(|error| {
                for store in stores {
                    store.close(key, incarnation);
                }
                MediaOutputError::WorkerSpawn(error)
            })?;
        Ok(Self { sender, closed })
    }

    #[must_use]
    pub fn try_enqueue(&self, event: MediaEvent, observed_at_unix_ms: u64) -> MediaEnqueueResult {
        if self.closed.load(Ordering::Acquire) {
            return MediaEnqueueResult::Closed;
        }
        match self.sender.try_send(MediaCommand::Event {
            event,
            observed_at_unix_ms,
        }) {
            Ok(()) => MediaEnqueueResult::Queued,
            Err(TrySendError::Full(_)) => MediaEnqueueResult::Dropped,
            Err(TrySendError::Disconnected(_)) => MediaEnqueueResult::Closed,
        }
    }

    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.sender.try_send(MediaCommand::Close);
    }
}

impl Drop for MediaPublisher {
    fn drop(&mut self) {
        self.close();
    }
}

fn run_worker(
    workers: &mut [MediaWorker],
    receiver: &Receiver<MediaCommand>,
    closed: &Arc<AtomicBool>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            MediaCommand::Event {
                event,
                observed_at_unix_ms,
            } => {
                for worker in workers.iter_mut() {
                    match worker {
                        MediaWorker::Hls(segmenter) => {
                            segmenter.accept(&event, observed_at_unix_ms)
                        }
                        MediaWorker::Dash(segmenter) => segmenter.accept(&event),
                    }
                }
                if closed.load(Ordering::Acquire) {
                    finish_workers(workers, true);
                    return;
                }
            }
            MediaCommand::Close => {
                finish_workers(workers, true);
                return;
            }
        }
    }
    finish_workers(workers, true);
}

fn finish_workers(workers: &mut [MediaWorker], end_list: bool) {
    for worker in workers {
        match worker {
            MediaWorker::Hls(segmenter) => segmenter.finish(end_list),
            MediaWorker::Dash(segmenter) => segmenter.finish(end_list),
        }
    }
}

struct HlsSegmenter {
    key: StreamKey,
    incarnation: PublisherIncarnation,
    store: Arc<MediaStore>,
    config: Arc<HlsOutputConfig>,
    variants: Vec<HlsVariant>,
    video_config: Option<AvcConfig>,
    audio_config: Option<AacConfig>,
    current: Option<SegmentBuilder>,
    published: VecDeque<PublishedSegment>,
    next_sequence: u64,
    current_key: Option<KeyMaterial>,
    failed: bool,
}

impl Drop for HlsSegmenter {
    fn drop(&mut self) {
        self.store.close(&self.key, self.incarnation);
    }
}

struct SegmentBuilder {
    start_timestamp_ms: u32,
    last_timestamp_ms: u32,
    muxer: TsMuxer,
    has_video: bool,
    has_audio: bool,
    too_large: bool,
}

struct PublishedSegment {
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
struct AvcConfig {
    nal_length_size: usize,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
}

#[derive(Clone, Copy)]
struct AacConfig {
    profile: u8,
    sample_rate_index: u8,
    channel_configuration: u8,
}

impl HlsSegmenter {
    fn new(
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

    fn accept(&mut self, event: &MediaEvent, observed_at_unix_ms: u64) {
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
            let elapsed = u64::from(timestamp_ms.saturating_sub(segment.start_timestamp_ms));
            let target =
                u64::try_from(self.config.segment_duration.as_millis()).unwrap_or(u64::MAX);
            let maximum =
                u64::try_from(self.config.max_segment_duration.as_millis()).unwrap_or(u64::MAX);
            elapsed >= target.min(maximum)
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

    fn finish(&mut self, end_list: bool) {
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
            || sequence % u64::try_from(rotation_segments).expect("bounded key rotation") == 0;
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

fn resolve_public_path(
    config: &HlsOutputConfig,
    prefix: &Path,
    object: &str,
) -> Result<(PathBuf, &'static str), MediaStoreError> {
    if object.is_empty() || object.len() > MAX_PUBLIC_OBJECT_BYTES || object.contains('%') {
        return Err(MediaStoreError::InvalidPath);
    }
    let components: Vec<_> = object.split('/').collect();
    if components
        .iter()
        .any(|component| safe_public_component(component).is_err())
    {
        return Err(MediaStoreError::InvalidPath);
    }
    let variants = config.variants();
    if object == "index.m3u8" {
        return Ok((
            if variants.len() > 1 {
                prefix.join("master.m3u8")
            } else {
                config
                    .variant_directory(prefix, &variants[0])
                    .join("index.m3u8")
            },
            "application/vnd.apple.mpegurl",
        ));
    }
    if object == "master.m3u8" && variants.len() > 1 {
        return Ok((prefix.join("master.m3u8"), "application/vnd.apple.mpegurl"));
    }
    for variant in &variants {
        let directory = config.variant_directory(prefix, variant);
        let variant_prefix = if directory == prefix {
            String::new()
        } else {
            format!("{}/", variant.name)
        };
        if let Some(relative) = object.strip_prefix(&variant_prefix) {
            if relative == "index.m3u8" {
                return Ok((directory.join(relative), "application/vnd.apple.mpegurl"));
            }
            if has_extension(relative, "ts") || has_extension(relative, "bin") {
                if let Some(key_prefix) = config.keys.as_ref().map(|keys| keys.url_prefix.as_str())
                {
                    if let Some(key_name) = relative.strip_prefix(key_prefix) {
                        if has_extension(key_name, "bin") {
                            return Ok((directory.join(key_name), content_type(key_name)));
                        }
                    }
                }
                if has_extension(relative, "bin") && config.keys.is_some() {
                    return Ok((directory.join(relative), content_type(relative)));
                }
                if has_extension(relative, "bin") {
                    return Err(MediaStoreError::NotFound);
                }
                return Ok((directory.join(relative), content_type(relative)));
            }
        }
    }
    Err(MediaStoreError::NotFound)
}

const MAX_PUBLIC_OBJECT_BYTES: usize = 1_024;

fn resolve_dash_public_path(
    config: &DashOutputConfig,
    prefix: &Path,
    object: &str,
) -> Result<(PathBuf, &'static str), MediaStoreError> {
    if object.is_empty() || object.len() > MAX_PUBLIC_OBJECT_BYTES || object.contains('%') {
        return Err(MediaStoreError::InvalidPath);
    }
    let components: Vec<_> = object.split('/').collect();
    if components
        .iter()
        .any(|component| safe_public_component(component).is_err())
    {
        return Err(MediaStoreError::InvalidPath);
    }
    let directory = config.media_directory(prefix);
    let relative = if config.nested {
        object.strip_prefix("dash/").unwrap_or(object)
    } else {
        object
    };
    if relative == "manifest.mpd" || relative == "index.mpd" {
        return Ok((directory.join("manifest.mpd"), "application/dash+xml"));
    }
    if relative == "init.mp4" {
        return Ok((directory.join(relative), "video/mp4"));
    }
    if relative.starts_with("seg-") && has_extension(relative, "m4s") {
        let sequence = relative
            .strip_suffix(".m4s")
            .and_then(|value| value.rsplit('-').next())
            .and_then(|value| value.parse::<u64>().ok());
        if relative.split('/').count() == 1 && sequence.is_some() {
            return Ok((directory.join(relative), "video/iso.segment"));
        }
    }
    Err(MediaStoreError::NotFound)
}

fn safe_public_component(component: &str) -> Result<(), MediaStoreError> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['\\', '\0'])
        || component.chars().any(char::is_control)
    {
        Err(MediaStoreError::InvalidPath)
    } else {
        Ok(())
    }
}

fn content_type(path: &str) -> &'static str {
    if has_extension(path, "ts") {
        "video/mp2t"
    } else if has_extension(path, "mpd") {
        "application/dash+xml"
    } else if has_extension(path, "mp4") || has_extension(path, "m4s") {
        "video/mp4"
    } else {
        "application/octet-stream"
    }
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

fn fragment_filename(naming: HlsFragmentNaming, sequence: u64, timestamp_ms: u32) -> String {
    match naming {
        HlsFragmentNaming::Sequential => format!("seg-{sequence}.ts"),
        HlsFragmentNaming::Timestamp => format!("seg-{timestamp_ms}.ts"),
        HlsFragmentNaming::System => format!("seg-{}-{sequence}.ts", system_time_ms()),
    }
}

fn system_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
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

fn parse_aac_config(payload: &[u8]) -> Option<AacConfig> {
    if payload.len() < 4 || payload[0] >> 4 != 10 || payload[1] != 0 {
        return None;
    }
    let first = payload[2];
    let second = payload[3];
    let object_type = first >> 3;
    let sample_rate_index = ((first & 0x07) << 1) | (second >> 7);
    let channel_configuration = (second >> 3) & 0x0f;
    if !(1..=4).contains(&object_type)
        || sample_rate_index == 15
        || sample_rate_index == 0
        || channel_configuration == 0
        || channel_configuration > 6
    {
        return None;
    }
    Some(AacConfig {
        profile: object_type.saturating_sub(1),
        sample_rate_index,
        channel_configuration,
    })
}

fn parse_avc_config(payload: &[u8]) -> Option<AvcConfig> {
    if payload.len() < 11 || payload[0] & 0x0f != 7 || payload[1] != 0 || payload[5] != 1 {
        return None;
    }
    let nal_length_size = usize::from(payload[9] & 0x03) + 1;
    let mut cursor = 10;
    let sps_count = usize::from(payload[cursor] & 0x1f);
    cursor += 1;
    let mut sps = Vec::with_capacity(sps_count.min(4));
    for _ in 0..sps_count {
        let length = usize::from(u16::from_be_bytes([
            *payload.get(cursor)?,
            *payload.get(cursor + 1)?,
        ]));
        cursor = cursor.checked_add(2)?;
        let end = cursor.checked_add(length)?;
        sps.push(payload.get(cursor..end)?.to_vec());
        cursor = end;
    }
    let pps_count = usize::from(*payload.get(cursor)?);
    cursor += 1;
    let mut pps = Vec::with_capacity(pps_count.min(4));
    for _ in 0..pps_count {
        let length = usize::from(u16::from_be_bytes([
            *payload.get(cursor)?,
            *payload.get(cursor + 1)?,
        ]));
        cursor = cursor.checked_add(2)?;
        let end = cursor.checked_add(length)?;
        pps.push(payload.get(cursor..end)?.to_vec());
        cursor = end;
    }
    if sps.is_empty() || pps.is_empty() {
        return None;
    }
    Some(AvcConfig {
        nal_length_size,
        sps,
        pps,
    })
}

struct TsMuxer {
    bytes: Vec<u8>,
    video: AvcConfig,
    audio: AacConfig,
    continuity: HashMap<u16, u8>,
}

impl TsMuxer {
    fn new(video: AvcConfig, audio: AacConfig) -> Self {
        let mut muxer = Self {
            bytes: Vec::with_capacity(TS_PACKET_BYTES * 16),
            video,
            audio,
            continuity: HashMap::new(),
        };
        muxer.write_pat();
        muxer.write_pmt();
        muxer
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn write_video(
        &mut self,
        event: &MediaEvent,
        timestamp_ms: u32,
        start_timestamp_ms: u32,
        keyframe: bool,
    ) -> Result<(), ()> {
        let payload = event.payload();
        if payload.len() < 9 || payload[1] != 1 {
            return Err(());
        }
        let composition_time = signed_24(&payload[2..5]);
        let mut access_unit = Vec::new();
        if keyframe {
            access_unit.extend_from_slice(&[0, 0, 0, 1, 0x09, 0xf0]);
            for nalu in self.video.sps.iter().chain(&self.video.pps) {
                access_unit.extend_from_slice(&[0, 0, 0, 1]);
                access_unit.extend_from_slice(nalu);
            }
        }
        append_length_prefixed_nalus(&mut access_unit, &payload[5..], self.video.nal_length_size)?;
        if access_unit.is_empty() {
            return Err(());
        }
        let dts = relative_timestamp(timestamp_ms, start_timestamp_ms);
        let pts =
            relative_timestamp_with_composition(timestamp_ms, composition_time, start_timestamp_ms);
        self.write_pes(VIDEO_PID, 0xe0, pts, dts, &access_unit);
        Ok(())
    }

    fn write_audio(
        &mut self,
        event: &MediaEvent,
        timestamp_ms: u32,
        start_timestamp_ms: u32,
    ) -> Result<(), ()> {
        let payload = event.payload();
        if payload.len() < 3 || payload[0] >> 4 != 10 || payload[1] != 1 {
            return Err(());
        }
        let raw = &payload[2..];
        let frame_length = raw.len().checked_add(7).ok_or(())?;
        if frame_length > 0x1fff {
            return Err(());
        }
        let mut adts = [0_u8; 7];
        adts[0] = 0xff;
        adts[1] = 0xf1;
        adts[2] = (self.audio.profile << 6)
            | (self.audio.sample_rate_index << 2)
            | (self.audio.channel_configuration >> 2);
        adts[3] = ((self.audio.channel_configuration & 0x03) << 6)
            | u8::try_from((frame_length >> 11) & 0x03).expect("ADTS length bits fit");
        adts[4] = u8::try_from((frame_length >> 3) & 0xff).expect("ADTS length bits fit");
        adts[5] = u8::try_from((frame_length & 0x07) << 5).expect("ADTS length bits fit") | 0x1f;
        adts[6] = 0xfc;
        let mut frame = Vec::with_capacity(frame_length);
        frame.extend_from_slice(&adts);
        frame.extend_from_slice(raw);
        let pts = relative_timestamp(timestamp_ms, start_timestamp_ms);
        self.write_pes(AUDIO_PID, 0xc0, pts, pts, &frame);
        Ok(())
    }

    fn write_pat(&mut self) {
        let mut section = vec![
            0x00,
            0xb0,
            0x0d,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0x00,
            0x01,
            0xe0 | 0x10,
            0x00,
        ];
        let crc = mpeg_crc32(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        self.write_psi(0, &section);
    }

    fn write_pmt(&mut self) {
        let mut section = vec![
            0x02,
            0xb0,
            0x17,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0xe0 | 0x01,
            0x01,
            0xf0,
            0x00,
            0x1b,
            0xe0 | 0x01,
            0x01,
            0xf0,
            0x00,
            0x0f,
            0xe0 | 0x01,
            0x02,
            0xf0,
            0x00,
        ];
        let crc = mpeg_crc32(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        self.write_psi(PMT_PID, &section);
    }

    fn write_psi(&mut self, pid: u16, section: &[u8]) {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        self.write_payload(pid, true, &payload);
    }

    fn write_pes(&mut self, pid: u16, stream_id: u8, pts: u64, dts: u64, payload: &[u8]) {
        let mut packet_bytes = Vec::with_capacity(payload.len() + 19);
        packet_bytes.extend_from_slice(&[0, 0, 1, stream_id]);
        let packet_length = if stream_id == 0xe0 {
            0
        } else {
            u16::try_from(payload.len().saturating_add(13)).unwrap_or(0)
        };
        packet_bytes.extend_from_slice(&packet_length.to_be_bytes());
        packet_bytes.extend_from_slice(&[0x80, 0x80, 5]);
        packet_bytes.extend_from_slice(&encode_pts(pts, false));
        packet_bytes.extend_from_slice(payload);
        let _ = dts;
        self.write_payload(pid, true, &packet_bytes);
    }

    fn write_payload(&mut self, pid: u16, payload_unit_start: bool, payload: &[u8]) {
        let mut continuity = *self.continuity.entry(pid).or_default();
        let mut offset = 0;
        while offset < payload.len() {
            let remaining = payload.len() - offset;
            let payload_bytes = remaining.min(184);
            let adaptation = payload_bytes < 184;
            let mut packet = [0xff_u8; TS_PACKET_BYTES];
            packet[0] = 0x47;
            packet[1] = u8::try_from((pid >> 8) & 0x1f).expect("PID bits fit")
                | u8::from(payload_unit_start && offset == 0) << 6;
            packet[2] = u8::try_from(pid & 0xff).expect("PID bits fit");
            packet[3] = (continuity & 0x0f) | if adaptation { 0x30 } else { 0x10 };
            continuity = continuity.wrapping_add(1) & 0x0f;
            if adaptation {
                let adaptation_length = 183 - payload_bytes;
                packet[4] = u8::try_from(adaptation_length).expect("adaptation length fits");
                packet[5] = 0;
                let payload_start = 5 + adaptation_length;
                packet[payload_start..payload_start + payload_bytes]
                    .copy_from_slice(&payload[offset..offset + payload_bytes]);
            } else {
                packet[4..4 + payload_bytes]
                    .copy_from_slice(&payload[offset..offset + payload_bytes]);
            }
            self.bytes.extend_from_slice(&packet);
            offset += payload_bytes;
        }
        self.continuity.insert(pid, continuity);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn append_length_prefixed_nalus(
    output: &mut Vec<u8>,
    bytes: &[u8],
    length_size: usize,
) -> Result<(), ()> {
    let mut cursor = 0;
    while cursor < bytes.len() {
        let end = cursor.checked_add(length_size).ok_or(())?;
        let length_bytes = bytes.get(cursor..end).ok_or(())?;
        let length = length_bytes
            .iter()
            .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
        cursor = end;
        let end = cursor.checked_add(length).ok_or(())?;
        let nalu = bytes.get(cursor..end).ok_or(())?;
        if !nalu.is_empty() {
            output.extend_from_slice(&[0, 0, 0, 1]);
            output.extend_from_slice(nalu);
        }
        cursor = end;
    }
    Ok(())
}

fn relative_timestamp(timestamp_ms: u32, start_timestamp_ms: u32) -> u64 {
    90_000_u64.saturating_add(u64::from(timestamp_ms.saturating_sub(start_timestamp_ms)) * 90)
}

fn relative_timestamp_with_composition(
    timestamp_ms: u32,
    composition_time: i32,
    start_timestamp_ms: u32,
) -> u64 {
    let timestamp = i64::from(timestamp_ms) + i64::from(composition_time);
    let start = i64::from(start_timestamp_ms);
    90_000_u64.saturating_add(u64::try_from((timestamp - start).max(0)).unwrap_or(0) * 90)
}

fn signed_24(bytes: &[u8]) -> i32 {
    let value = (i32::from(bytes[0]) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[2]);
    if value & 0x80_0000 != 0 {
        value | !0x00ff_ffff
    } else {
        value
    }
}

fn encode_pts(value: u64, marker: bool) -> [u8; 5] {
    let value = value & 0x1fff_fffff;
    [
        (u8::from(marker) << 4)
            | u8::try_from(((value >> 30) & 0x07) << 1).expect("PTS bits fit")
            | 1,
        u8::try_from((value >> 22) & 0xff).expect("PTS bits fit"),
        u8::try_from(((value >> 15) & 0x7f) << 1).expect("PTS bits fit") | 1,
        u8::try_from((value >> 7) & 0xff).expect("PTS bits fit"),
        u8::try_from((value & 0x7f) << 1).expect("PTS bits fit") | 1,
    ]
}

fn mpeg_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::*;
    use crate::{DashSegmentNaming, LiveHub, LiveHubLimits, MediaEvent, MediaStoreLimits};

    fn test_config(store: Arc<MediaStore>) -> Arc<HlsOutputConfig> {
        Arc::new(HlsOutputConfig {
            store,
            segment_duration: Duration::from_secs(2),
            max_segment_duration: Duration::from_secs(10),
            playlist_length: Duration::from_secs(30),
            naming: HlsFragmentNaming::Sequential,
            nested: false,
            cleanup: true,
            variants: Vec::new(),
            keys: Some(HlsKeyConfig {
                rotation_segments: 1,
                url_prefix: "keys/".into(),
            }),
            max_segment_bytes: 1024 * 1024,
            max_queue_messages: 16,
        })
    }

    fn avc_sequence_header() -> Vec<u8> {
        vec![
            0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1, 0, 2,
            0x68, 0xce,
        ]
    }

    fn avc_frame(keyframe: bool) -> Vec<u8> {
        vec![
            if keyframe { 0x17 } else { 0x27 },
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            0x65,
            0x88,
            0x84,
            0x21,
        ]
    }

    #[test]
    fn cuts_ts_segments_and_serves_encrypted_keys_from_the_playlist_path() {
        let root = tempdir().expect("temporary media root");
        let store = Arc::new(
            MediaStore::open(
                root.path().join("hls"),
                MediaStoreLimits {
                    max_bytes: 1024 * 1024,
                    max_files: 32,
                    max_active_streams: 2,
                    max_file_bytes: 1024 * 1024,
                },
            )
            .expect("media store"),
        );
        let hub = LiveHub::new(LiveHubLimits::default());
        let key = StreamKey::new("service", "application", "stream");
        let lease = hub.attach_publisher(key.clone()).expect("publisher");
        store
            .attach(&key, lease.incarnation())
            .expect("media incarnation");
        let mut segmenter = HlsSegmenter::new(
            key.clone(),
            lease.incarnation(),
            Arc::clone(&store),
            test_config(Arc::clone(&store)),
        );
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x12, 0x10].as_slice()))
                .expect("AAC sequence header"),
            0,
        );
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header()))
                .expect("AVC sequence header"),
            0,
        );
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(avc_frame(true))).expect("keyframe"),
            0,
        );
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 1, 1, 2, 3].as_slice()))
                .expect("AAC frame"),
            0,
        );
        segmenter.accept(
            &MediaEvent::audio(1_000, Arc::<[u8]>::from([0xaf, 1, 4, 5, 6].as_slice()))
                .expect("AAC frame"),
            1_000,
        );
        segmenter.accept(
            &MediaEvent::video(1_000, Arc::<[u8]>::from(avc_frame(false))).expect("interframe"),
            1_000,
        );
        segmenter.accept(
            &MediaEvent::video(2_000, Arc::<[u8]>::from(avc_frame(true)))
                .expect("segment boundary keyframe"),
            2_000,
        );
        segmenter.finish(true);

        let prefix = store.current_prefix(&key).expect("current media prefix");
        let playlist = store
            .read_relative(&prefix.join("index.m3u8"), 1024)
            .expect("playlist");
        let playlist = String::from_utf8(playlist).expect("playlist UTF-8");
        assert!(playlist.contains("#EXTINF:"));
        assert!(playlist.contains("#EXT-X-ENDLIST"));
        assert!(playlist.contains("URI=\"keys/key-0.bin\""));
        let (key_path, _) =
            resolve_public_path(&test_config(Arc::clone(&store)), &prefix, "keys/key-0.bin")
                .expect("key path");
        assert_eq!(store.read_relative(&key_path, 16).expect("key").len(), 16);
        let segment_path = prefix.join("seg-0.ts");
        let segment = store
            .read_relative(&segment_path, 1024 * 1024)
            .expect("segment");
        assert!(!segment.is_empty() && segment.len() % 16 == 0);
    }

    #[test]
    fn rejects_public_path_traversal() {
        let root = tempdir().expect("temporary media root");
        let store = Arc::new(
            MediaStore::open(
                root.path().join("hls"),
                MediaStoreLimits {
                    max_bytes: 1024,
                    max_files: 8,
                    max_active_streams: 1,
                    max_file_bytes: 512,
                },
            )
            .expect("media store"),
        );
        let config = test_config(store);
        let prefix = PathBuf::from("application/stream/i1");
        assert!(matches!(
            resolve_public_path(&config, &prefix, "../secret.ts"),
            Err(MediaStoreError::InvalidPath)
        ));
    }

    #[test]
    fn releases_attached_stores_when_a_second_output_cannot_attach() {
        let root = tempdir().expect("temporary media root");
        let limits = MediaStoreLimits {
            max_bytes: 1024 * 1024,
            max_files: 16,
            max_active_streams: 1,
            max_file_bytes: 1024 * 1024,
        };
        let hls_store =
            Arc::new(MediaStore::open(root.path().join("hls"), limits).expect("HLS store"));
        let dash_store =
            Arc::new(MediaStore::open(root.path().join("dash"), limits).expect("DASH store"));
        let hub = LiveHub::new(LiveHubLimits::default());
        let occupied_key = StreamKey::new("service", "application", "occupied");
        let occupied = hub
            .attach_publisher(occupied_key.clone())
            .expect("occupied publisher");
        dash_store
            .attach(&occupied_key, occupied.incarnation())
            .expect("occupied DASH incarnation");

        let key = StreamKey::new("service", "application", "stream");
        let lease = hub.attach_publisher(key.clone()).expect("publisher");
        let dash = Arc::new(DashOutputConfig {
            store: Arc::clone(&dash_store),
            segment_duration: Duration::from_secs(1),
            max_segment_duration: Duration::from_secs(2),
            playlist_length: Duration::from_secs(4),
            naming: DashSegmentNaming::Sequential,
            nested: true,
            cleanup: true,
            max_segment_bytes: 1024 * 1024,
            max_queue_messages: 16,
        });
        let application =
            MediaApplication::new(Some(test_config(Arc::clone(&hls_store)))).with_dash(Some(dash));
        assert!(matches!(
            MediaPublisher::start(&key, lease.incarnation(), &application),
            Err(MediaOutputError::Storage(
                MediaStoreError::ActiveStreamLimit
            ))
        ));
        assert!(hls_store.current_prefix(&key).is_none());
        dash_store.close(&occupied_key, occupied.incarnation());
    }
}
