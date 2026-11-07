#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpMediaPlan {
    hls: Option<RtmpHlsPlan>,
    dash: Option<RtmpDashPlan>,
}

impl RtmpMediaPlan {
    pub fn new(
        hls: Option<RtmpHlsPlan>,
        dash: Option<RtmpDashPlan>,
    ) -> Result<Self, RtmpPrepareError> {
        if hls.is_none() && dash.is_none() {
            return Err(value("media.outputs"));
        }
        Ok(Self { hls, dash })
    }
    #[must_use]
    pub const fn hls(&self) -> Option<&RtmpHlsPlan> {
        self.hls.as_ref()
    }
    #[must_use]
    pub const fn dash(&self) -> Option<&RtmpDashPlan> {
        self.dash.as_ref()
    }

    /// Combines acquired HLS and DASH outputs into one media application.
    #[must_use]
    pub(crate) fn combine_outputs(
        hls: Option<Arc<HlsOutputConfig>>,
        dash: Option<Arc<DashOutputConfig>>,
    ) -> Option<Arc<MediaApplication>> {
        match (hls, dash) {
            (None, None) => None,
            (Some(hls), None) => Some(Arc::new(MediaApplication::new(Some(hls)))),
            (None, Some(dash)) => Some(Arc::new(MediaApplication::new(None).with_dash(Some(dash)))),
            (Some(hls), Some(dash)) => Some(Arc::new(
                MediaApplication::new(Some(hls)).with_dash(Some(dash)),
            )),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpHlsPlan {
    root_directory: PathBuf,
    segment_duration: Duration,
    max_segment_duration: Duration,
    playlist_length: Duration,
    naming: HlsFragmentNaming,
    nested: bool,
    cleanup: bool,
    variants: Vec<HlsVariant>,
    keys: Option<HlsKeyConfig>,
    max_segment_bytes: usize,
    max_queue_messages: usize,
    max_storage_bytes: u64,
    max_storage_files: usize,
    max_active_streams: usize,
}

impl RtmpHlsPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root_directory: PathBuf,
        segment_duration: Duration,
        max_segment_duration: Duration,
        playlist_length: Duration,
        naming: HlsFragmentNaming,
        nested: bool,
        cleanup: bool,
        variants: impl IntoIterator<Item = HlsVariant>,
        keys: Option<HlsKeyConfig>,
        max_segment_bytes: usize,
        max_queue_messages: usize,
        max_storage_bytes: u64,
        max_storage_files: usize,
        max_active_streams: usize,
    ) -> Result<Self, RtmpPrepareError> {
        validate_media_bounds(
            &root_directory,
            segment_duration,
            max_segment_duration,
            playlist_length,
            max_segment_bytes,
            max_queue_messages,
            max_storage_bytes,
            max_storage_files,
            max_active_streams,
        )?;
        let variants: Vec<_> = variants.into_iter().collect();
        for variant in &variants {
            variant
                .validate_intrinsic()
                .map_err(|source| value("hls.variants").source(source))?;
        }
        if let Some(keys) = &keys {
            keys.validate_intrinsic()
                .map_err(|source| value("hls.keys").source(source))?;
        }
        Ok(Self {
            root_directory,
            segment_duration,
            max_segment_duration,
            playlist_length,
            naming,
            nested,
            cleanup,
            variants,
            keys,
            max_segment_bytes,
            max_queue_messages,
            max_storage_bytes,
            max_storage_files,
            max_active_streams,
        })
    }
    #[must_use]
    pub fn root_directory(&self) -> &Path {
        &self.root_directory
    }
    #[must_use]
    pub fn variants(&self) -> &[HlsVariant] {
        &self.variants
    }
    #[must_use]
    pub const fn keys(&self) -> Option<&HlsKeyConfig> {
        self.keys.as_ref()
    }
    #[must_use]
    pub const fn segment_duration(&self) -> Duration {
        self.segment_duration
    }
    #[must_use]
    pub const fn max_segment_duration(&self) -> Duration {
        self.max_segment_duration
    }
    #[must_use]
    pub const fn playlist_length(&self) -> Duration {
        self.playlist_length
    }
    #[must_use]
    pub const fn naming(&self) -> HlsFragmentNaming {
        self.naming
    }
    #[must_use]
    pub const fn nested(&self) -> bool {
        self.nested
    }
    #[must_use]
    pub const fn cleanup(&self) -> bool {
        self.cleanup
    }
    #[must_use]
    pub const fn max_segment_bytes(&self) -> usize {
        self.max_segment_bytes
    }
    #[must_use]
    pub const fn max_queue_messages(&self) -> usize {
        self.max_queue_messages
    }
    #[must_use]
    pub const fn max_storage_bytes(&self) -> u64 {
        self.max_storage_bytes
    }
    #[must_use]
    pub const fn max_storage_files(&self) -> usize {
        self.max_storage_files
    }
    #[must_use]
    pub const fn max_active_streams(&self) -> usize {
        self.max_active_streams
    }

    #[must_use]
    pub const fn media_store_limits(&self) -> MediaStoreLimits {
        MediaStoreLimits {
            max_bytes: self.max_storage_bytes,
            max_files: self.max_storage_files,
            max_active_streams: self.max_active_streams,
            max_file_bytes: self.max_segment_bytes,
        }
    }

    /// Builds the HLS output from an already-open media store.
    #[must_use]
    pub(crate) fn build_output(&self, store: Arc<MediaStore>) -> Arc<HlsOutputConfig> {
        Arc::new(HlsOutputConfig {
            store,
            segment_duration: self.segment_duration,
            max_segment_duration: self.max_segment_duration,
            playlist_length: self.playlist_length,
            naming: self.naming,
            nested: self.nested,
            cleanup: self.cleanup,
            variants: self.variants.clone(),
            keys: self.keys.clone(),
            max_segment_bytes: self.max_segment_bytes,
            max_queue_messages: self.max_queue_messages,
        })
    }
}

impl fmt::Debug for RtmpHlsPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpHlsPlan")
            .field("root_directory", &"<redacted>")
            .field("segment_duration", &self.segment_duration)
            .field("max_segment_duration", &self.max_segment_duration)
            .field("playlist_length", &self.playlist_length)
            .field("naming", &self.naming)
            .field("nested", &self.nested)
            .field("cleanup", &self.cleanup)
            .field("variants", &self.variants)
            .field("keys", &self.keys)
            .field("max_segment_bytes", &self.max_segment_bytes)
            .field("max_queue_messages", &self.max_queue_messages)
            .field("max_storage_bytes", &self.max_storage_bytes)
            .field("max_storage_files", &self.max_storage_files)
            .field("max_active_streams", &self.max_active_streams)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpDashPlan {
    root_directory: PathBuf,
    segment_duration: Duration,
    max_segment_duration: Duration,
    playlist_length: Duration,
    naming: DashSegmentNaming,
    nested: bool,
    cleanup: bool,
    max_segment_bytes: usize,
    max_queue_messages: usize,
    max_storage_bytes: u64,
    max_storage_files: usize,
    max_active_streams: usize,
}

impl RtmpDashPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root_directory: PathBuf,
        segment_duration: Duration,
        max_segment_duration: Duration,
        playlist_length: Duration,
        naming: DashSegmentNaming,
        nested: bool,
        cleanup: bool,
        max_segment_bytes: usize,
        max_queue_messages: usize,
        max_storage_bytes: u64,
        max_storage_files: usize,
        max_active_streams: usize,
    ) -> Result<Self, RtmpPrepareError> {
        validate_media_bounds(
            &root_directory,
            segment_duration,
            max_segment_duration,
            playlist_length,
            max_segment_bytes,
            max_queue_messages,
            max_storage_bytes,
            max_storage_files,
            max_active_streams,
        )?;
        Ok(Self {
            root_directory,
            segment_duration,
            max_segment_duration,
            playlist_length,
            naming,
            nested,
            cleanup,
            max_segment_bytes,
            max_queue_messages,
            max_storage_bytes,
            max_storage_files,
            max_active_streams,
        })
    }
    #[must_use]
    pub fn root_directory(&self) -> &Path {
        &self.root_directory
    }
    #[must_use]
    pub const fn segment_duration(&self) -> Duration {
        self.segment_duration
    }
    #[must_use]
    pub const fn max_segment_duration(&self) -> Duration {
        self.max_segment_duration
    }
    #[must_use]
    pub const fn playlist_length(&self) -> Duration {
        self.playlist_length
    }
    #[must_use]
    pub const fn naming(&self) -> DashSegmentNaming {
        self.naming
    }
    #[must_use]
    pub const fn nested(&self) -> bool {
        self.nested
    }
    #[must_use]
    pub const fn cleanup(&self) -> bool {
        self.cleanup
    }
    #[must_use]
    pub const fn max_segment_bytes(&self) -> usize {
        self.max_segment_bytes
    }
    #[must_use]
    pub const fn max_queue_messages(&self) -> usize {
        self.max_queue_messages
    }
    #[must_use]
    pub const fn max_storage_bytes(&self) -> u64 {
        self.max_storage_bytes
    }
    #[must_use]
    pub const fn max_storage_files(&self) -> usize {
        self.max_storage_files
    }
    #[must_use]
    pub const fn max_active_streams(&self) -> usize {
        self.max_active_streams
    }

    #[must_use]
    pub const fn media_store_limits(&self) -> MediaStoreLimits {
        MediaStoreLimits {
            max_bytes: self.max_storage_bytes,
            max_files: self.max_storage_files,
            max_active_streams: self.max_active_streams,
            max_file_bytes: self.max_segment_bytes,
        }
    }

    /// Builds the DASH output from an already-open media store.
    #[must_use]
    pub(crate) fn build_output(&self, store: Arc<MediaStore>) -> Arc<DashOutputConfig> {
        Arc::new(DashOutputConfig {
            store,
            segment_duration: self.segment_duration,
            max_segment_duration: self.max_segment_duration,
            playlist_length: self.playlist_length,
            naming: self.naming,
            nested: self.nested,
            cleanup: self.cleanup,
            max_segment_bytes: self.max_segment_bytes,
            max_queue_messages: self.max_queue_messages,
        })
    }
}

impl fmt::Debug for RtmpDashPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpDashPlan")
            .field("root_directory", &"<redacted>")
            .field("segment_duration", &self.segment_duration)
            .field("max_segment_duration", &self.max_segment_duration)
            .field("playlist_length", &self.playlist_length)
            .field("naming", &self.naming)
            .field("nested", &self.nested)
            .field("cleanup", &self.cleanup)
            .field("max_segment_bytes", &self.max_segment_bytes)
            .field("max_queue_messages", &self.max_queue_messages)
            .field("max_storage_bytes", &self.max_storage_bytes)
            .field("max_storage_files", &self.max_storage_files)
            .field("max_active_streams", &self.max_active_streams)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpRecorderPlan {
    name: String,
    start: RtmpRecorderStart,
    root_directory: PathBuf,
    path_policy: RecordingPathPolicy,
    worker: RecorderWorkerConfig,
    store_limits: RecordingStoreLimits,
}

impl RtmpRecorderPlan {
    pub fn new(
        name: impl Into<String>,
        start: RtmpRecorderStart,
        root_directory: PathBuf,
        path_policy: RecordingPathPolicy,
        worker: RecorderWorkerConfig,
        store_limits: RecordingStoreLimits,
    ) -> Result<Self, RtmpPrepareError> {
        let name = name.into();
        validate_identity(&name, "recorder.name").map_err(|error| error.with_recorder(&name))?;
        if !valid_absolute_path(&root_directory) {
            return Err(value("recorder.root_directory").with_recorder(&name));
        }
        worker
            .validate_intrinsic()
            .map_err(|source| value("recorder.worker").with_recorder(&name).source(source))?;
        store_limits
            .validate_intrinsic()
            .map_err(|source| bound(source.field).with_recorder(&name).source(source))?;
        Ok(Self {
            name,
            start,
            root_directory,
            path_policy,
            worker,
            store_limits,
        })
    }
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub const fn start(&self) -> RtmpRecorderStart {
        self.start
    }
    #[must_use]
    pub fn root_directory(&self) -> &Path {
        &self.root_directory
    }
    #[must_use]
    pub const fn path_policy(&self) -> &RecordingPathPolicy {
        &self.path_policy
    }
    #[must_use]
    pub const fn worker(&self) -> RecorderWorkerConfig {
        self.worker
    }
    #[must_use]
    pub const fn store_limits(&self) -> RecordingStoreLimits {
        self.store_limits
    }

    /// Builds this recorder's runtime policy from an already-open shared store.
    #[must_use]
    pub(crate) fn build_policy(&self, store: crate::RecordingStore) -> RtmpRecorderPolicy {
        RtmpRecorderPolicy::new(
            &self.name,
            self.start,
            store,
            self.path_policy.clone(),
            self.worker,
        )
    }
}

impl fmt::Debug for RtmpRecorderPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpRecorderPlan")
            .field("name", &self.name)
            .field("start", &self.start)
            .field("root_directory", &"<redacted>")
            .field("path_policy", &self.path_policy)
            .field("worker", &self.worker)
            .field("store_limits", &self.store_limits)
            .finish()
    }
}
