#![allow(clippy::missing_errors_doc)]

use std::{
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    DashOutputConfig, DashSegmentNaming, DestinationPolicyError, ExecEnvironment,
    ExecFilesystemPolicy, ExecLimits, ExecMode, ExecNetworkPolicy, ExecProfile, ExecProfileError,
    ExecTrigger, HlsFragmentNaming, HlsKeyConfig, HlsOutputConfig, HlsValueError, HlsVariant,
    LiveHub, LiveHubLimits, MediaApplication, MediaStore, MediaStoreLimits, RecorderWorkerConfig,
    RecorderWorkerStartError, RecordingPathPolicy, RecordingStoreLimits, RecordingStoreLimitsError,
    RtmpAccessAction, RtmpAccessPolicy, RtmpAccessRule, RtmpApplication, RtmpAutoPushConfig,
    RtmpAutoPushConfigError, RtmpCallbackMethod, RtmpCallbackPolicy, RtmpCallbackValueError,
    RtmpNetwork, RtmpOutboundPolicy, RtmpPullTarget, RtmpPushApplication, RtmpPushTarget,
    RtmpRecorderPolicy, RtmpRecorderStart, RtmpSessionCeilings, RtmpSessionLimitError,
    RtmpSessionLimits, RtmpStreamPath, RtmpTokenPolicy, RtmpTransport, VodApplication, VodLimits,
    VodSourceDefinition, VodValueError, validate_callback_url_intrinsic,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpPrepareMode {
    Activation,
    Validation,
}

/// Value-only preparation inputs. Candidate addresses are sorted and deduplicated; they are not
/// reserved, bound, or checked against the host network here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPrepareContext {
    mode: RtmpPrepareMode,
    candidate_listener_addresses: Vec<SocketAddr>,
}

impl RtmpPrepareContext {
    #[must_use]
    pub fn new(
        mode: RtmpPrepareMode,
        candidate_listener_addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> Self {
        let mut candidate_listener_addresses: Vec<_> =
            candidate_listener_addresses.into_iter().collect();
        candidate_listener_addresses.sort_unstable();
        candidate_listener_addresses.dedup();
        Self {
            mode,
            candidate_listener_addresses,
        }
    }

    #[must_use]
    pub const fn mode(&self) -> RtmpPrepareMode {
        self.mode
    }

    #[must_use]
    pub fn candidate_listener_addresses(&self) -> &[SocketAddr] {
        &self.candidate_listener_addresses
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RtmpPrepareCategory {
    Identity,
    Bound,
    Value,
}

#[derive(Debug)]
pub struct RtmpPrepareError {
    category: RtmpPrepareCategory,
    field: &'static str,
    context: Box<RtmpPrepareErrorContext>,
    source: Option<Box<RtmpPrepareSource>>,
}

impl fmt::Display for RtmpPrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid RTMP ")?;
        write!(formatter, "{:?} at {}", self.category, self.field)?;
        if let Some(service) = &self.context.service {
            write!(formatter, " in service `{service}`")?;
        }
        if let Some(application) = &self.context.application {
            write!(formatter, ", application `{application}`")?;
        }
        if let Some(recorder) = &self.context.recorder {
            write!(formatter, ", recorder `{recorder}`")?;
        }
        if let Some(profile) = &self.context.profile {
            write!(formatter, ", profile `{profile}`")?;
        }
        Ok(())
    }
}

impl std::error::Error for RtmpPrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug, Default)]
struct RtmpPrepareErrorContext {
    service: Option<String>,
    application: Option<String>,
    recorder: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RtmpPrepareSource {
    #[error(transparent)]
    OutboundPolicy(#[from] DestinationPolicyError),
    #[error(transparent)]
    Session(#[from] RtmpSessionLimitError),
    #[error(transparent)]
    Exec(#[from] ExecProfileError),
    #[error(transparent)]
    RecorderWorker(#[from] RecorderWorkerStartError),
    #[error(transparent)]
    RecordingStore(#[from] RecordingStoreLimitsError),
    #[error(transparent)]
    Hls(#[from] HlsValueError),
    #[error(transparent)]
    AutoPush(#[from] RtmpAutoPushConfigError),
    #[error(transparent)]
    Callback(#[from] RtmpCallbackValueError),
    #[error(transparent)]
    Vod(#[from] VodValueError),
}

impl RtmpPrepareError {
    fn new(category: RtmpPrepareCategory, field: &'static str) -> Self {
        Self {
            category,
            field,
            context: Box::default(),
            source: None,
        }
    }

    #[must_use]
    pub fn with_service(mut self, value: impl Into<String>) -> Self {
        self.context.service = Some(value.into());
        self
    }
    #[must_use]
    pub fn with_application(mut self, value: impl Into<String>) -> Self {
        self.context.application = Some(value.into());
        self
    }
    #[must_use]
    pub fn with_recorder(mut self, value: impl Into<String>) -> Self {
        self.context.recorder = Some(value.into());
        self
    }
    #[must_use]
    pub fn with_profile(mut self, value: impl Into<String>) -> Self {
        self.context.profile = Some(value.into());
        self
    }
    #[must_use]
    pub fn contextualize_service(self, value: impl Into<String>) -> Self {
        self.with_service(value)
    }
    #[must_use]
    pub fn contextualize_application(self, value: impl Into<String>) -> Self {
        self.with_application(value)
    }
    #[must_use]
    pub fn contextualize_recorder(self, value: impl Into<String>) -> Self {
        self.with_recorder(value)
    }
    #[must_use]
    pub fn contextualize_profile(self, value: impl Into<String>) -> Self {
        self.with_profile(value)
    }
    fn source(mut self, value: impl Into<RtmpPrepareSource>) -> Self {
        self.source = Some(Box::new(value.into()));
        self
    }

    #[must_use]
    pub const fn category(&self) -> RtmpPrepareCategory {
        self.category
    }
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }
    #[must_use]
    pub fn service_id(&self) -> Option<&str> {
        self.context.service.as_deref()
    }
    #[must_use]
    pub fn application_name(&self) -> Option<&str> {
        self.context.application.as_deref()
    }
    #[must_use]
    pub fn recorder_name(&self) -> Option<&str> {
        self.context.recorder.as_deref()
    }
    #[must_use]
    pub fn profile_name(&self) -> Option<&str> {
        self.context.profile.as_deref()
    }
    #[must_use]
    pub fn prepare_source(&self) -> Option<&RtmpPrepareSource> {
        self.source.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpServicePlan {
    service_id: String,
    outbound_chunk_size: u32,
    inbound_limits: RtmpSessionLimits,
    outbound_policy: RtmpOutboundPolicy,
    callbacks: RtmpCallbackPlan,
    applications: Vec<RtmpApplicationPlan>,
    auto_push: Option<RtmpAutoPushPlan>,
}

impl RtmpServicePlan {
    pub fn new(
        service_id: impl Into<String>,
        outbound_chunk_size: u32,
        inbound_limits: RtmpSessionLimits,
        callbacks: RtmpCallbackPlan,
        applications: impl IntoIterator<Item = RtmpApplicationPlan>,
        auto_push: Option<RtmpAutoPushPlan>,
    ) -> Result<Self, RtmpPrepareError> {
        let service_id = service_id.into();
        validate_identity(&service_id, "service.id")
            .map_err(|error| error.with_service(&service_id))?;
        if outbound_chunk_size == 0 {
            return Err(bound("service.outbound_chunk_size").with_service(&service_id));
        }
        inbound_limits.validate_intrinsic().map_err(|source| {
            bound("service.inbound_limits")
                .with_service(&service_id)
                .source(source)
        })?;
        let applications: Vec<_> = applications.into_iter().collect();
        Ok(Self {
            service_id,
            outbound_chunk_size,
            inbound_limits,
            outbound_policy: RtmpOutboundPolicy::default(),
            callbacks,
            applications,
            auto_push,
        })
    }

    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }
    #[must_use]
    pub const fn outbound_chunk_size(&self) -> u32 {
        self.outbound_chunk_size
    }
    #[must_use]
    pub const fn inbound_limits(&self) -> RtmpSessionLimits {
        self.inbound_limits
    }
    pub fn with_outbound_policy(
        mut self,
        outbound_policy: RtmpOutboundPolicy,
    ) -> Result<Self, RtmpPrepareError> {
        outbound_policy
            .validate_cidrs_intrinsic()
            .map_err(|source| value("service.outbound_policy.cidrs").source(source))?;
        self.outbound_policy = outbound_policy;
        Ok(self)
    }
    #[must_use]
    pub const fn outbound_policy(&self) -> &RtmpOutboundPolicy {
        &self.outbound_policy
    }
    #[must_use]
    pub const fn callbacks(&self) -> &RtmpCallbackPlan {
        &self.callbacks
    }
    #[must_use]
    pub fn applications(&self) -> &[RtmpApplicationPlan] {
        &self.applications
    }
    #[must_use]
    pub const fn auto_push(&self) -> Option<&RtmpAutoPushPlan> {
        self.auto_push.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpApplicationPlan {
    name: String,
    live: bool,
    idle_streams: bool,
    publish: RtmpAccessPlan,
    play: RtmpAccessPlan,
    session_limits: RtmpSessionCeilings,
    fanout: RtmpFanoutPlan,
    relay: RtmpRelayPlan,
    media: Option<RtmpMediaPlan>,
    recorders: Vec<RtmpRecorderPlan>,
    vod: Option<RtmpVodPlan>,
    callbacks: RtmpCallbackPlan,
    exec: Vec<RtmpExecPlan>,
}

impl RtmpApplicationPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        live: bool,
        idle_streams: bool,
        publish: RtmpAccessPlan,
        play: RtmpAccessPlan,
        session_limits: RtmpSessionCeilings,
        fanout: RtmpFanoutPlan,
        relay: RtmpRelayPlan,
        media: Option<RtmpMediaPlan>,
        recorders: impl IntoIterator<Item = RtmpRecorderPlan>,
        vod: Option<RtmpVodPlan>,
        callbacks: RtmpCallbackPlan,
        exec: impl IntoIterator<Item = RtmpExecPlan>,
    ) -> Result<Self, RtmpPrepareError> {
        let name = name.into();
        validate_application(&name, "application.name")
            .map_err(|error| error.with_application(&name))?;
        session_limits.validate_intrinsic().map_err(|source| {
            bound("application.session_limits")
                .with_application(&name)
                .source(source)
        })?;
        let recorders: Vec<_> = recorders.into_iter().collect();
        let exec: Vec<_> = exec.into_iter().collect();
        Ok(Self {
            name,
            live,
            idle_streams,
            publish,
            play,
            session_limits,
            fanout,
            relay,
            media,
            recorders,
            vod,
            callbacks,
            exec,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub const fn live(&self) -> bool {
        self.live
    }
    #[must_use]
    pub const fn idle_streams(&self) -> bool {
        self.idle_streams
    }
    #[must_use]
    pub const fn publish(&self) -> &RtmpAccessPlan {
        &self.publish
    }
    #[must_use]
    pub const fn play(&self) -> &RtmpAccessPlan {
        &self.play
    }
    #[must_use]
    pub const fn session_limits(&self) -> RtmpSessionCeilings {
        self.session_limits
    }
    #[must_use]
    pub const fn fanout(&self) -> RtmpFanoutPlan {
        self.fanout
    }
    #[must_use]
    pub const fn relay(&self) -> &RtmpRelayPlan {
        &self.relay
    }
    #[must_use]
    pub const fn media(&self) -> Option<&RtmpMediaPlan> {
        self.media.as_ref()
    }
    #[must_use]
    pub fn recorders(&self) -> &[RtmpRecorderPlan] {
        &self.recorders
    }
    #[must_use]
    pub const fn vod(&self) -> Option<&RtmpVodPlan> {
        self.vod.as_ref()
    }
    #[must_use]
    pub const fn callbacks(&self) -> &RtmpCallbackPlan {
        &self.callbacks
    }
    #[must_use]
    pub fn exec(&self) -> &[RtmpExecPlan] {
        &self.exec
    }

    /// Assembles the runtime application from resources acquired by RTMP preparation.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn build_runtime_application(
        &self,
        hub: LiveHub,
        push_targets: impl IntoIterator<Item = RtmpPushTarget>,
        pull_targets: impl IntoIterator<Item = RtmpPullTarget>,
        callbacks: RtmpCallbackPolicy,
        vod: Option<Arc<VodApplication>>,
        media: Option<Arc<MediaApplication>>,
        exec_profiles: impl IntoIterator<Item = ExecProfile>,
        recorders: impl IntoIterator<Item = RtmpRecorderPolicy>,
    ) -> RtmpApplication {
        RtmpApplication::with_runtime(
            self.name.clone(),
            self.live,
            self.idle_streams,
            hub,
            push_targets,
            recorders,
        )
        .with_pull_targets(pull_targets)
        .with_vod(vod)
        .with_media(media)
        .with_exec_profiles(exec_profiles)
        .with_callbacks(callbacks)
        .with_authorization(
            self.publish.runtime_policy(),
            self.play.runtime_policy(),
            self.session_limits,
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RtmpAccessPlan {
    rules: Vec<RtmpAccessRulePlan>,
    token: Option<RtmpTokenPlan>,
}

impl RtmpAccessPlan {
    #[must_use]
    pub fn new(
        rules: impl IntoIterator<Item = RtmpAccessRulePlan>,
        token: Option<RtmpTokenPlan>,
    ) -> Self {
        Self {
            rules: rules.into_iter().collect(),
            token,
        }
    }
    #[must_use]
    pub fn rules(&self) -> &[RtmpAccessRulePlan] {
        &self.rules
    }
    #[must_use]
    pub const fn token(&self) -> Option<&RtmpTokenPlan> {
        self.token.as_ref()
    }

    #[must_use]
    pub fn runtime_policy(&self) -> RtmpAccessPolicy {
        RtmpAccessPolicy::new(
            self.rules
                .iter()
                .map(|rule| RtmpAccessRule::new(rule.action(), rule.network().clone())),
            self.token.as_ref().map(|token| token.policy().clone()),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpAccessRulePlan {
    rule: RtmpAccessRule,
}

impl RtmpAccessRulePlan {
    pub fn new(action: RtmpAccessAction, network: RtmpNetwork) -> Result<Self, RtmpPrepareError> {
        if !network.is_representable() {
            return Err(value("access.network"));
        }
        Ok(Self {
            rule: RtmpAccessRule::new(action, network),
        })
    }
    #[must_use]
    pub const fn action(&self) -> RtmpAccessAction {
        self.rule.action()
    }
    #[must_use]
    pub const fn network(&self) -> &RtmpNetwork {
        self.rule.network()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpTokenPlan {
    policy: RtmpTokenPolicy,
}

impl RtmpTokenPlan {
    pub fn new(
        parameter: impl Into<String>,
        secret: impl AsRef<[u8]>,
    ) -> Result<Self, RtmpPrepareError> {
        let parameter = parameter.into();
        let policy = RtmpTokenPolicy::stream_query(parameter, secret)
            .ok_or_else(|| value("access.token"))?;
        Ok(Self { policy })
    }
    #[must_use]
    pub fn parameter(&self) -> &str {
        self.policy.parameter()
    }
    #[must_use]
    pub const fn policy(&self) -> &RtmpTokenPolicy {
        &self.policy
    }
}

impl fmt::Debug for RtmpTokenPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpTokenPlan")
            .field("parameter", &self.parameter())
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpFanoutPlan {
    max_subscribers: usize,
    max_queue_messages_per_subscriber: usize,
    max_queue_bytes_per_subscriber: usize,
}

impl RtmpFanoutPlan {
    pub fn new(
        max_subscribers: usize,
        max_queue_messages_per_subscriber: usize,
        max_queue_bytes_per_subscriber: usize,
    ) -> Result<Self, RtmpPrepareError> {
        if max_subscribers == 0
            || max_queue_messages_per_subscriber == 0
            || max_queue_bytes_per_subscriber == 0
            || max_subscribers
                .checked_mul(max_queue_bytes_per_subscriber)
                .is_none()
        {
            return Err(bound("fanout"));
        }
        Ok(Self {
            max_subscribers,
            max_queue_messages_per_subscriber,
            max_queue_bytes_per_subscriber,
        })
    }
    #[must_use]
    pub const fn max_subscribers(self) -> usize {
        self.max_subscribers
    }
    #[must_use]
    pub const fn max_queue_messages_per_subscriber(self) -> usize {
        self.max_queue_messages_per_subscriber
    }
    #[must_use]
    pub const fn max_queue_bytes_per_subscriber(self) -> usize {
        self.max_queue_bytes_per_subscriber
    }

    /// Builds the live fanout hub represented by this validated plan.
    #[must_use]
    pub fn runtime_hub(self) -> LiveHub {
        LiveHub::new(LiveHubLimits {
            max_subscribers: self.max_subscribers,
            max_subscribers_per_stream: self.max_subscribers,
            max_queue_messages_per_subscriber: self.max_queue_messages_per_subscriber,
            max_queue_bytes_per_subscriber: self.max_queue_bytes_per_subscriber,
            max_fanout_bytes: self.max_subscribers * self.max_queue_bytes_per_subscriber,
            ..LiveHubLimits::default()
        })
    }
}

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
    pub fn combine_outputs(
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
    pub fn build_output(&self, store: Arc<MediaStore>) -> Arc<HlsOutputConfig> {
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
    pub fn build_output(&self, store: Arc<MediaStore>) -> Arc<DashOutputConfig> {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpRelayPlan {
    policy: RtmpOutboundPolicy,
    max_queue_messages: usize,
    max_queue_bytes: usize,
    buffer_duration: Duration,
    push_reconnect_interval: Duration,
    pull_reconnect_interval: Duration,
    dns_refresh_interval: Duration,
    connect_timeout: Duration,
    handshake_timeout: Duration,
    push: Vec<RtmpPushPlan>,
    pull: Vec<RtmpPullPlan>,
}

impl RtmpRelayPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        policy: RtmpOutboundPolicy,
        max_queue_messages: usize,
        max_queue_bytes: usize,
        buffer_duration: Duration,
        push_reconnect_interval: Duration,
        pull_reconnect_interval: Duration,
        dns_refresh_interval: Duration,
        connect_timeout: Duration,
        handshake_timeout: Duration,
        push: impl IntoIterator<Item = RtmpPushPlan>,
        pull: impl IntoIterator<Item = RtmpPullPlan>,
    ) -> Result<Self, RtmpPrepareError> {
        policy
            .validate_cidrs_intrinsic()
            .map_err(|source| value("relay.policy.cidrs").source(source))?;
        if max_queue_messages == 0
            || max_queue_bytes == 0
            || buffer_duration.is_zero()
            || push_reconnect_interval.is_zero()
            || pull_reconnect_interval.is_zero()
            || dns_refresh_interval.is_zero()
            || connect_timeout.is_zero()
            || handshake_timeout.is_zero()
        {
            return Err(bound("relay.bounds"));
        }
        Ok(Self {
            policy,
            max_queue_messages,
            max_queue_bytes,
            buffer_duration,
            push_reconnect_interval,
            pull_reconnect_interval,
            dns_refresh_interval,
            connect_timeout,
            handshake_timeout,
            push: push.into_iter().collect(),
            pull: pull.into_iter().collect(),
        })
    }
    #[must_use]
    pub const fn policy(&self) -> &RtmpOutboundPolicy {
        &self.policy
    }
    #[must_use]
    pub fn push(&self) -> &[RtmpPushPlan] {
        &self.push
    }
    #[must_use]
    pub fn pull(&self) -> &[RtmpPullPlan] {
        &self.pull
    }
    #[must_use]
    pub const fn max_queue_messages(&self) -> usize {
        self.max_queue_messages
    }
    #[must_use]
    pub const fn max_queue_bytes(&self) -> usize {
        self.max_queue_bytes
    }
    #[must_use]
    pub const fn buffer_duration(&self) -> Duration {
        self.buffer_duration
    }
    #[must_use]
    pub const fn push_reconnect_interval(&self) -> Duration {
        self.push_reconnect_interval
    }
    #[must_use]
    pub const fn pull_reconnect_interval(&self) -> Duration {
        self.pull_reconnect_interval
    }
    #[must_use]
    pub const fn dns_refresh_interval(&self) -> Duration {
        self.dns_refresh_interval
    }
    #[must_use]
    pub const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }
    #[must_use]
    pub const fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    /// Resolves and assembles one push target without connecting to it.
    ///
    /// # Errors
    ///
    /// Returns a destination error when startup resolution, policy, family selection, or direct
    /// listener-loop validation fails.
    pub fn acquire_push_target<E>(
        &self,
        target: &RtmpPushPlan,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        acquire_options: impl FnOnce() -> Result<crate::RtmpClientOptions, E>,
        map_destination_error: impl FnOnce(crate::RtmpDestinationResolverError) -> E,
    ) -> Result<RtmpPushTarget, E> {
        let resolver = self
            .acquire_resolver(
                target.host(),
                target.port(),
                target.transport(),
                listener_addresses,
            )
            .map_err(map_destination_error)?;
        let options = acquire_options()?;
        Ok(RtmpPushTarget {
            address: resolver.address(),
            host: target.host().to_owned(),
            transport: target.transport(),
            application: target.application().clone(),
            stream_name: target.stream_name().map(str::to_owned),
            options,
            config: self.runtime_config(self.push_reconnect_interval, resolver),
        })
    }

    /// Resolves and assembles one pull target without connecting to it.
    ///
    /// # Errors
    ///
    /// Returns a destination error when startup resolution, policy, family selection, or direct
    /// listener-loop validation fails.
    pub fn acquire_pull_target<E>(
        &self,
        target: &RtmpPullPlan,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        acquire_options: impl FnOnce() -> Result<crate::RtmpClientOptions, E>,
        map_destination_error: impl FnOnce(crate::RtmpDestinationResolverError) -> E,
    ) -> Result<RtmpPullTarget, E> {
        let resolver = self
            .acquire_resolver(
                target.host(),
                target.port(),
                target.transport(),
                listener_addresses,
            )
            .map_err(map_destination_error)?;
        let options = acquire_options()?;
        Ok(RtmpPullTarget {
            address: resolver.address(),
            host: target.host().to_owned(),
            transport: target.transport(),
            source_application: target.source_application().to_owned(),
            source_stream_name: target.source_stream_name().to_owned(),
            local_application: target.local_application().to_owned(),
            local_stream_name: target.local_stream_name().to_owned(),
            options,
            config: self.runtime_config(self.pull_reconnect_interval, resolver),
        })
    }

    fn acquire_resolver(
        &self,
        host: &str,
        port: u16,
        transport: RtmpTransport,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> Result<crate::RtmpDestinationResolver, crate::RtmpDestinationResolverError> {
        let addresses = crate::RtmpDestinationResolver::resolve_startup_addresses(host, port)
            .map_err(|_| crate::RtmpDestinationResolverError::EmptyAnswer)?;
        crate::RtmpDestinationResolver::from_startup(
            host.to_owned(),
            port,
            transport,
            addresses,
            self.policy.clone(),
            listener_addresses,
            self.dns_refresh_interval,
        )
    }

    fn runtime_config(
        &self,
        reconnect_interval: Duration,
        resolver: crate::RtmpDestinationResolver,
    ) -> crate::RtmpRelayConfig {
        crate::RtmpRelayConfig {
            max_queue_messages: self.max_queue_messages,
            max_queue_bytes: self.max_queue_bytes,
            buffer_duration: self.buffer_duration,
            connect_timeout: self.connect_timeout,
            handshake_timeout: self.handshake_timeout,
            reconnect_interval,
            max_chain_depth: self.policy.max_chain_depth,
            dns_resolver: Some(Arc::new(resolver)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPushPlan {
    host: String,
    port: u16,
    transport: RtmpTransport,
    application: RtmpPushApplication,
    stream_name: Option<String>,
    client: RtmpClientPlan,
}

impl RtmpPushPlan {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        transport: RtmpTransport,
        application: RtmpPushApplication,
        stream_name: Option<String>,
        client: RtmpClientPlan,
    ) -> Result<Self, RtmpPrepareError> {
        let host = host.into();
        validate_destination(&host, port)?;
        if let RtmpPushApplication::Exact(application) = &application {
            validate_application(application, "relay.push.application")?;
        }
        if let Some(stream_name) = &stream_name {
            validate_stream_name(stream_name, "relay.push.stream_name")?;
        }
        Ok(Self {
            host,
            port,
            transport,
            application,
            stream_name,
            client,
        })
    }
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
    #[must_use]
    pub const fn client(&self) -> &RtmpClientPlan {
        &self.client
    }
    #[must_use]
    pub const fn transport(&self) -> RtmpTransport {
        self.transport
    }
    #[must_use]
    pub const fn application(&self) -> &RtmpPushApplication {
        &self.application
    }
    #[must_use]
    pub fn stream_name(&self) -> Option<&str> {
        self.stream_name.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPullPlan {
    host: String,
    port: u16,
    transport: RtmpTransport,
    source_application: String,
    source_stream_name: String,
    local_application: String,
    local_stream_name: String,
    client: RtmpClientPlan,
}

impl RtmpPullPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: impl Into<String>,
        port: u16,
        transport: RtmpTransport,
        source_application: impl Into<String>,
        source_stream_name: impl Into<String>,
        local_application: impl Into<String>,
        local_stream_name: impl Into<String>,
        client: RtmpClientPlan,
    ) -> Result<Self, RtmpPrepareError> {
        let host = host.into();
        validate_destination(&host, port)?;
        let source_application = source_application.into();
        let source_stream_name = source_stream_name.into();
        let local_application = local_application.into();
        let local_stream_name = local_stream_name.into();
        validate_application(&source_application, "relay.pull.source_application")?;
        validate_stream_name(&source_stream_name, "relay.pull.source_stream_name")?;
        validate_application(&local_application, "relay.pull.local_application")?;
        validate_stream_name(&local_stream_name, "relay.pull.local_stream_name")?;
        Ok(Self {
            host,
            port,
            transport,
            source_application,
            source_stream_name,
            local_application,
            local_stream_name,
            client,
        })
    }
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
    #[must_use]
    pub const fn client(&self) -> &RtmpClientPlan {
        &self.client
    }
    #[must_use]
    pub const fn transport(&self) -> RtmpTransport {
        self.transport
    }
    #[must_use]
    pub fn source_application(&self) -> &str {
        &self.source_application
    }
    #[must_use]
    pub fn source_stream_name(&self) -> &str {
        &self.source_stream_name
    }
    #[must_use]
    pub fn local_application(&self) -> &str {
        &self.local_application
    }
    #[must_use]
    pub fn local_stream_name(&self) -> &str {
        &self.local_stream_name
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpClientPlan {
    flash_version: String,
    playback_buffer_ms: u32,
    tc_url: Option<String>,
    credential: Option<RtmpCredentialPlan>,
}

impl RtmpClientPlan {
    pub fn new(
        flash_version: impl Into<String>,
        playback_buffer_ms: u32,
        tc_url: Option<String>,
        credential: Option<RtmpCredentialPlan>,
    ) -> Result<Self, RtmpPrepareError> {
        let flash_version = flash_version.into();
        if flash_version.is_empty()
            || flash_version.len() > 128
            || flash_version.chars().any(char::is_control)
        {
            return Err(value("relay.client.flash_version"));
        }
        if tc_url.as_ref().is_some_and(|url| {
            url.len() > 2_048
                || (!url.starts_with("rtmp://") && !url.starts_with("rtmps://"))
                || url.contains([' ', '\n', '\r', '#'])
        }) {
            return Err(value("relay.client.tc_url"));
        }
        Ok(Self {
            flash_version,
            playback_buffer_ms,
            tc_url,
            credential,
        })
    }
    #[must_use]
    pub fn flash_version(&self) -> &str {
        &self.flash_version
    }
    #[must_use]
    pub const fn playback_buffer_ms(&self) -> u32 {
        self.playback_buffer_ms
    }
    #[must_use]
    pub fn tc_url(&self) -> Option<&str> {
        self.tc_url.as_deref()
    }
    #[must_use]
    pub const fn credential(&self) -> Option<&RtmpCredentialPlan> {
        self.credential.as_ref()
    }
}

impl fmt::Debug for RtmpClientPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpClientPlan")
            .field("flash_version", &self.flash_version)
            .field("playback_buffer_ms", &self.playback_buffer_ms)
            .field("tc_url", &self.tc_url.as_ref().map(|_| "<redacted>"))
            .field("credential", &self.credential)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpCredentialPlan {
    username: String,
    secret_file: PathBuf,
}
impl RtmpCredentialPlan {
    pub fn new(
        username: impl Into<String>,
        secret_file: PathBuf,
    ) -> Result<Self, RtmpPrepareError> {
        let username = username.into();
        if username.is_empty()
            || username.len() > 128
            || username.chars().any(char::is_control)
            || !valid_absolute_file_path(&secret_file)
        {
            return Err(value("relay.credential"));
        }
        Ok(Self {
            username,
            secret_file,
        })
    }
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }
    #[must_use]
    pub fn secret_file(&self) -> &Path {
        &self.secret_file
    }
}
impl fmt::Debug for RtmpCredentialPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpCredentialPlan")
            .field("username", &self.username)
            .field("secret_file", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpAutoPushPlan {
    config: RtmpAutoPushConfig,
}
impl RtmpAutoPushPlan {
    pub fn new(config: RtmpAutoPushConfig) -> Result<Self, RtmpPrepareError> {
        config
            .validate_intrinsic()
            .map_err(|source| value("auto_push.config").source(source))?;
        Ok(Self { config })
    }
    #[must_use]
    pub const fn config(&self) -> &RtmpAutoPushConfig {
        &self.config
    }
}
impl fmt::Debug for RtmpAutoPushPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpAutoPushPlan")
            .field("enabled", &self.config.enabled)
            .field("socket_dir", &"<redacted>")
            .field(
                "secret_file",
                &self.config.secret_file.as_ref().map(|_| "<redacted>"),
            )
            .field("reconnect_interval", &self.config.reconnect_interval)
            .field("connect_timeout", &self.config.connect_timeout)
            .field("handshake_timeout", &self.config.handshake_timeout)
            .field("max_peers", &self.config.max_peers)
            .field("max_queue_messages", &self.config.max_queue_messages)
            .field("max_queue_bytes", &self.config.max_queue_bytes)
            .field("max_streams", &self.config.max_streams)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpVodPlan {
    limits: VodLimits,
    sources: Vec<VodSourceDefinition>,
    outbound_policy: RtmpOutboundPolicy,
}
impl RtmpVodPlan {
    pub fn new(
        limits: VodLimits,
        sources: impl IntoIterator<Item = VodSourceDefinition>,
        outbound_policy: RtmpOutboundPolicy,
    ) -> Result<Self, RtmpPrepareError> {
        outbound_policy
            .validate_cidrs_intrinsic()
            .map_err(|source| value("vod.outbound_policy.cidrs").source(source))?;
        limits
            .validate_intrinsic()
            .map_err(|source| bound("vod.limits").source(source))?;
        let sources: Vec<_> = sources.into_iter().collect();
        for source in &sources {
            source
                .validate_intrinsic()
                .map_err(|source| value("vod.sources").source(source))?;
        }
        Ok(Self {
            limits,
            sources,
            outbound_policy,
        })
    }
    #[must_use]
    pub const fn limits(&self) -> VodLimits {
        self.limits
    }
    #[must_use]
    pub fn sources(&self) -> &[VodSourceDefinition] {
        &self.sources
    }
    #[must_use]
    pub const fn outbound_policy(&self) -> &RtmpOutboundPolicy {
        &self.outbound_policy
    }
    /// Opens local roots and acquires HTTP origins for one VOD application.
    ///
    /// # Errors
    ///
    /// Returns an acquisition error when a source root or origin cannot be prepared.
    pub fn acquire(
        &self,
        service: impl Into<String>,
        application: impl Into<String>,
    ) -> Result<crate::VodApplication, crate::VodError> {
        let blueprint = crate::VodApplicationBlueprint::compile(
            service,
            application,
            self.limits,
            self.sources.clone(),
            &self.outbound_policy,
        )?;
        crate::VodApplication::acquire(&blueprint)
    }
}
impl fmt::Debug for RtmpVodPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpVodPlan")
            .field("limits", &self.limits)
            .field(
                "sources",
                &format_args!("<{} redacted>", self.sources.len()),
            )
            .field("outbound_policy", &self.outbound_policy)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpCallbackPlan {
    endpoints: [Option<String>; 8],
    method: RtmpCallbackMethod,
    timeout: Duration,
    update_timeout: Duration,
    update_strict: bool,
    relay_redirect: bool,
}
impl Default for RtmpCallbackPlan {
    fn default() -> Self {
        Self {
            endpoints: Default::default(),
            method: RtmpCallbackMethod::default(),
            timeout: Duration::from_secs(10),
            update_timeout: Duration::from_secs(30),
            update_strict: false,
            relay_redirect: false,
        }
    }
}
impl RtmpCallbackPlan {
    pub fn new(
        method: RtmpCallbackMethod,
        timeout: Duration,
        update_timeout: Duration,
    ) -> Result<Self, RtmpPrepareError> {
        if timeout.is_zero() {
            return Err(bound("callbacks.timeout"));
        }
        Ok(Self {
            method,
            timeout,
            update_timeout,
            ..Self::default()
        })
    }
    pub fn with_endpoint(
        mut self,
        event: RtmpCallbackEventPlan,
        url: impl Into<String>,
    ) -> Result<Self, RtmpPrepareError> {
        let url = url.into();
        validate_callback_url_intrinsic(&url)
            .map_err(|source| value("callbacks.url").source(source))?;
        self.endpoints[event as usize] = Some(url);
        Ok(self)
    }
    #[must_use]
    pub const fn with_update_policy(mut self, strict: bool, relay_redirect: bool) -> Self {
        self.update_strict = strict;
        self.relay_redirect = relay_redirect;
        self
    }
    #[must_use]
    pub fn endpoint(&self, event: RtmpCallbackEventPlan) -> Option<&str> {
        self.endpoints[event as usize].as_deref()
    }
    /// Parses, resolves, and policy-checks one configured callback endpoint.
    ///
    /// # Errors
    ///
    /// Returns a redacted callback error when the endpoint is malformed, cannot be resolved, or
    /// fails the configured outbound destination policy.
    pub fn acquire_endpoint(
        &self,
        event: RtmpCallbackEventPlan,
        outbound_policy: &RtmpOutboundPolicy,
    ) -> Result<Option<crate::RtmpCallbackEndpoint>, crate::RtmpCallbackError> {
        self.endpoint(event)
            .map(|value| crate::RtmpCallbackEndpoint::parse(value, outbound_policy))
            .transpose()
    }
    #[must_use]
    pub const fn method(&self) -> RtmpCallbackMethod {
        self.method
    }
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
    #[must_use]
    pub const fn update_timeout(&self) -> Duration {
        self.update_timeout
    }
    #[must_use]
    pub const fn update_strict(&self) -> bool {
        self.update_strict
    }
    #[must_use]
    pub const fn relay_redirect(&self) -> bool {
        self.relay_redirect
    }
}
impl fmt::Debug for RtmpCallbackPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoints = self
            .endpoints
            .each_ref()
            .map(|value| value.as_ref().map(|_| "<redacted>"));
        f.debug_struct("RtmpCallbackPlan")
            .field("endpoints", &endpoints)
            .field("method", &self.method)
            .field("timeout", &self.timeout)
            .field("update_timeout", &self.update_timeout)
            .field("update_strict", &self.update_strict)
            .field("relay_redirect", &self.relay_redirect)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RtmpCallbackEventPlan {
    Connect,
    Disconnect,
    Publish,
    PublishDone,
    Play,
    PlayDone,
    Done,
    Update,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpExecPlan {
    profile: ExecProfile,
}
impl RtmpExecPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        application: impl Into<String>,
        mode: ExecMode,
        trigger: ExecTrigger,
        executable: PathBuf,
        arguments: impl IntoIterator<Item = String>,
        environment: impl IntoIterator<Item = RtmpExecEnvironmentPlan>,
        working_directory: PathBuf,
        filesystem: ExecFilesystemPolicy,
        network: ExecNetworkPolicy,
        limits: ExecLimits,
        respawn: bool,
    ) -> Result<Self, RtmpPrepareError> {
        let name = name.into();
        let environment = environment
            .into_iter()
            .map(|value| value.entry)
            .collect::<Vec<_>>();
        let profile = ExecProfile::new(
            name.clone(),
            application,
            mode,
            trigger,
            executable,
            arguments,
            environment,
            working_directory,
            filesystem,
            network,
            limits,
            respawn,
        )
        .map_err(|error| exec_error(&error).with_profile(name).source(error))?;
        Ok(Self { profile })
    }
    #[must_use]
    pub fn name(&self) -> &str {
        self.profile.name()
    }
    #[must_use]
    pub fn application(&self) -> &str {
        self.profile.application()
    }
    #[must_use]
    pub const fn profile(&self) -> &ExecProfile {
        &self.profile
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpExecEnvironmentPlan {
    entry: ExecEnvironment,
}
impl RtmpExecEnvironmentPlan {
    pub fn new(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, RtmpPrepareError> {
        Ok(Self {
            entry: ExecEnvironment::new(name, value)
                .map_err(|error| exec_error(&error).source(error))?,
        })
    }
    #[must_use]
    pub fn name(&self) -> &str {
        self.entry.name()
    }
    #[must_use]
    pub fn value(&self) -> &str {
        self.entry.value()
    }
}
impl fmt::Debug for RtmpExecEnvironmentPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.entry.fmt(f)
    }
}

fn exec_error(error: &ExecProfileError) -> RtmpPrepareError {
    let field = match error {
        ExecProfileError::InvalidName => "exec.name",
        ExecProfileError::InvalidApplication => "exec.application",
        ExecProfileError::InvalidExecutable | ExecProfileError::ShellExecutable => {
            "exec.executable"
        }
        ExecProfileError::InvalidWorkingDirectory => "exec.working_directory",
        ExecProfileError::InvalidArguments => "exec.arguments",
        ExecProfileError::InvalidEnvironment => "exec.environment",
        ExecProfileError::InvalidLimit(field) => field,
        ExecProfileError::InvalidTranscodeTrigger => "exec.trigger",
    };
    value(field)
}
fn validate_identity(value: &str, field: &'static str) -> Result<(), RtmpPrepareError> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        Err(RtmpPrepareError::new(RtmpPrepareCategory::Identity, field))
    } else {
        Ok(())
    }
}

fn validate_application(value: &str, field: &'static str) -> Result<(), RtmpPrepareError> {
    RtmpStreamPath::validate_application(value)
        .map_err(|_| RtmpPrepareError::new(RtmpPrepareCategory::Identity, field))
}

fn validate_stream_name(value: &str, field: &'static str) -> Result<(), RtmpPrepareError> {
    RtmpStreamPath::parse("application", value)
        .map(|_| ())
        .map_err(|_| RtmpPrepareError::new(RtmpPrepareCategory::Identity, field))
}
#[allow(clippy::too_many_arguments)]
fn validate_media_bounds(
    root: &Path,
    segment: Duration,
    max_segment: Duration,
    playlist: Duration,
    max_bytes: usize,
    max_messages: usize,
    max_storage_bytes: u64,
    max_storage_files: usize,
    max_streams: usize,
) -> Result<(), RtmpPrepareError> {
    if !valid_absolute_path(root)
        || segment.is_zero()
        || max_segment < segment
        || playlist < segment
        || max_bytes == 0
        || max_messages == 0
        || max_storage_bytes == 0
        || max_storage_files == 0
        || max_streams == 0
    {
        Err(bound("media.bounds"))
    } else {
        Ok(())
    }
}

fn valid_absolute_file_path(path: &Path) -> bool {
    valid_absolute_path(path) && path != Path::new("/")
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 4_096
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            ) && component.as_os_str().to_str().is_some_and(|value| {
                !value
                    .bytes()
                    .any(|byte| byte == 0 || byte.is_ascii_control())
            })
        })
}
fn validate_destination(host: &str, port: u16) -> Result<(), RtmpPrepareError> {
    if host.is_empty()
        || host.len() > 253
        || host.trim() != host
        || host.chars().any(char::is_control)
        || port == 0
    {
        Err(value("relay.destination"))
    } else {
        Ok(())
    }
}
fn bound(field: &'static str) -> RtmpPrepareError {
    RtmpPrepareError::new(RtmpPrepareCategory::Bound, field)
}
fn value(field: &'static str) -> RtmpPrepareError {
    RtmpPrepareError::new(RtmpPrepareCategory::Value, field)
}
