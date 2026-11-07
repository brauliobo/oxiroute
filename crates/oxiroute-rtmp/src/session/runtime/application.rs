#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub struct RtmpApplication {
    name: String,
    live: bool,
    idle_streams: bool,
    publish_policy: RtmpAccessPolicy,
    play_policy: RtmpAccessPolicy,
    session_limits: RtmpSessionCeilings,
    pub(super) admission: Arc<ApplicationAdmission>,
    hub: Option<LiveHub>,
    push_targets: Arc<Vec<RtmpPushTarget>>,
    pub(super) pull_targets: Arc<Vec<RtmpPullTarget>>,
    callbacks: Arc<RtmpCallbackPolicy>,
    vod: Option<Arc<VodApplication>>,
    media: Option<Arc<MediaApplication>>,
    exec_profiles: Option<Arc<ExecProfileSet>>,
    pub(super) recorders: Arc<Vec<RtmpRecorderPolicy>>,
}

impl RtmpApplication {
    #[must_use]
    pub fn new(name: impl Into<String>, live: bool, idle_streams: bool) -> Self {
        let session_limits = RtmpSessionCeilings::default();
        Self {
            name: name.into(),
            live,
            idle_streams,
            publish_policy: RtmpAccessPolicy::default(),
            play_policy: RtmpAccessPolicy::default(),
            session_limits,
            admission: Arc::new(ApplicationAdmission::new(session_limits)),
            hub: None,
            push_targets: Arc::new(Vec::new()),
            pull_targets: Arc::new(Vec::new()),
            callbacks: Arc::new(RtmpCallbackPolicy::default()),
            vod: None,
            media: None,
            exec_profiles: None,
            recorders: Arc::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn with_recorders(
        name: impl Into<String>,
        live: bool,
        idle_streams: bool,
        recorders: impl IntoIterator<Item = RtmpRecorderPolicy>,
    ) -> Self {
        let session_limits = RtmpSessionCeilings::default();
        Self {
            name: name.into(),
            live,
            idle_streams,
            publish_policy: RtmpAccessPolicy::default(),
            play_policy: RtmpAccessPolicy::default(),
            session_limits,
            admission: Arc::new(ApplicationAdmission::new(session_limits)),
            hub: None,
            push_targets: Arc::new(Vec::new()),
            pull_targets: Arc::new(Vec::new()),
            callbacks: Arc::new(RtmpCallbackPolicy::default()),
            vod: None,
            media: None,
            exec_profiles: None,
            recorders: Arc::new(recorders.into_iter().collect()),
        }
    }

    #[must_use]
    pub fn with_runtime(
        name: impl Into<String>,
        live: bool,
        idle_streams: bool,
        hub: LiveHub,
        push_targets: impl IntoIterator<Item = RtmpPushTarget>,
        recorders: impl IntoIterator<Item = RtmpRecorderPolicy>,
    ) -> Self {
        let session_limits = RtmpSessionCeilings::default();
        Self {
            name: name.into(),
            live,
            idle_streams,
            publish_policy: RtmpAccessPolicy::default(),
            play_policy: RtmpAccessPolicy::default(),
            session_limits,
            admission: Arc::new(ApplicationAdmission::new(session_limits)),
            hub: Some(hub),
            push_targets: Arc::new(push_targets.into_iter().collect()),
            pull_targets: Arc::new(Vec::new()),
            callbacks: Arc::new(RtmpCallbackPolicy::default()),
            vod: None,
            media: None,
            exec_profiles: None,
            recorders: Arc::new(recorders.into_iter().collect()),
        }
    }

    #[must_use]
    pub fn with_authorization(
        mut self,
        publish_policy: RtmpAccessPolicy,
        play_policy: RtmpAccessPolicy,
        session_limits: RtmpSessionCeilings,
    ) -> Self {
        self.publish_policy = publish_policy;
        self.play_policy = play_policy;
        self.session_limits = session_limits;
        self.admission = Arc::new(ApplicationAdmission::new(session_limits));
        self
    }

    #[must_use]
    pub fn with_pull_targets(
        mut self,
        pull_targets: impl IntoIterator<Item = RtmpPullTarget>,
    ) -> Self {
        self.pull_targets = Arc::new(pull_targets.into_iter().collect());
        self
    }

    #[must_use]
    pub fn with_vod(mut self, vod: Option<Arc<VodApplication>>) -> Self {
        self.vod = vod;
        self
    }

    #[must_use]
    pub fn with_media(mut self, media: Option<Arc<MediaApplication>>) -> Self {
        self.media = media;
        self
    }

    #[must_use]
    pub fn with_exec_profiles(mut self, profiles: impl IntoIterator<Item = ExecProfile>) -> Self {
        self.exec_profiles = ExecProfileSet::new(profiles);
        self
    }

    #[must_use]
    pub fn with_callbacks(mut self, callbacks: RtmpCallbackPolicy) -> Self {
        self.callbacks = Arc::new(callbacks);
        self
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
    pub fn recorder_policies(&self) -> &[RtmpRecorderPolicy] {
        &self.recorders
    }

    #[must_use]
    pub fn push_targets(&self) -> &[RtmpPushTarget] {
        &self.push_targets
    }

    #[must_use]
    pub fn pull_targets(&self) -> &[RtmpPullTarget] {
        &self.pull_targets
    }

    #[must_use]
    pub fn callbacks(&self) -> &RtmpCallbackPolicy {
        &self.callbacks
    }

    #[must_use]
    pub fn vod(&self) -> Option<Arc<VodApplication>> {
        self.vod.clone()
    }

    #[must_use]
    pub fn media(&self) -> Option<Arc<MediaApplication>> {
        self.media.clone()
    }

    pub(crate) fn exec_profiles(&self) -> Option<Arc<ExecProfileSet>> {
        self.exec_profiles.clone()
    }

    #[must_use]
    pub const fn session_limits(&self) -> RtmpSessionCeilings {
        self.session_limits
    }

    pub(super) fn hub(&self) -> Option<LiveHub> {
        self.hub.clone()
    }

    pub(super) fn policy(&self, operation: SessionOperation) -> &RtmpAccessPolicy {
        match operation {
            SessionOperation::Publish => &self.publish_policy,
            SessionOperation::Play => &self.play_policy,
        }
    }
}

#[derive(Clone, Copy)]
pub(in super::super) enum SessionOperation {
    Publish,
    Play,
}

#[derive(Clone)]
pub struct RtmpSessionPolicy {
    pub(super) applications: Arc<BTreeMap<String, RtmpApplication>>,
    outbound_chunk_size: u32,
    inbound_limits: RtmpSessionLimits,
}

impl RtmpSessionPolicy {
    #[must_use]
    pub fn new(applications: impl IntoIterator<Item = RtmpApplication>) -> Self {
        Self {
            applications: Arc::new(
                applications
                    .into_iter()
                    .map(|application| (application.name.clone(), application))
                    .collect(),
            ),
            outbound_chunk_size: 4_096,
            inbound_limits: RtmpSessionLimits::default(),
        }
    }

    #[must_use]
    pub fn with_outbound_chunk_size(
        applications: impl IntoIterator<Item = RtmpApplication>,
        outbound_chunk_size: u32,
    ) -> Self {
        let mut policy = Self::new(applications);
        policy.outbound_chunk_size = outbound_chunk_size;
        policy
    }

    #[must_use]
    pub fn with_inbound_limits(
        applications: impl IntoIterator<Item = RtmpApplication>,
        inbound_limits: RtmpSessionLimits,
    ) -> Self {
        let mut policy = Self::new(applications);
        policy.inbound_limits = inbound_limits;
        policy
    }

    #[must_use]
    pub fn with_session_limits(
        applications: impl IntoIterator<Item = RtmpApplication>,
        outbound_chunk_size: u32,
        inbound_limits: RtmpSessionLimits,
    ) -> Self {
        Self {
            applications: Arc::new(
                applications
                    .into_iter()
                    .map(|application| (application.name.clone(), application))
                    .collect(),
            ),
            outbound_chunk_size,
            inbound_limits,
        }
    }

    pub(super) fn application(&self, name: &str) -> Option<&RtmpApplication> {
        self.applications.get(name)
    }

    pub(in super::super) const fn outbound_chunk_size(&self) -> u32 {
        self.outbound_chunk_size
    }

    pub(in super::super) const fn inbound_limits(&self) -> RtmpSessionLimits {
        self.inbound_limits
    }

    #[cfg(test)]
    pub(super) fn first_hub(&self) -> Option<LiveHub> {
        self.applications.values().find_map(RtmpApplication::hub)
    }
}

impl Default for RtmpSessionPolicy {
    fn default() -> Self {
        Self::new([])
    }
}

#[allow(clippy::large_enum_variant)]
pub(in super::super) enum SessionRole {
    Publisher(PublishSession),
    Playback(PlaybackSession),
    VodPlayback(VodPlaybackSession),
}

impl SessionRole {
    pub(in super::super) fn identity(&self) -> (&str, &str) {
        match self {
            Self::Publisher(publisher) => (publisher.application(), publisher.stream_name()),
            Self::Playback(playback) => (playback.application(), playback.stream_name()),
            Self::VodPlayback(playback) => (playback.application(), playback.stream_name()),
        }
    }

    pub(in super::super) fn observe_at(&mut self, at_unix_ms: u64) {
        match self {
            Self::Publisher(publisher) => publisher.observe_at(at_unix_ms),
            Self::Playback(playback) => playback.observe_at(at_unix_ms),
            Self::VodPlayback(_) => {}
        }
    }

    pub(in super::super) fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        match self {
            Self::Publisher(publisher) => publisher.release(at_unix_ms),
            Self::Playback(playback) => playback.release(at_unix_ms),
            Self::VodPlayback(playback) => {
                playback.release();
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
pub(in super::super) enum PublisherRoleError {
    AdmissionClosed,
    SessionLimit(SessionLimitError),
    Hub(LiveHubError),
    Catalog(CatalogError),
    AutoPush(RtmpAutoPushError),
}

#[derive(Debug)]
pub(in super::super) enum PlaybackRoleError {
    AdmissionClosed,
    NoPublisher,
    SessionLimit(SessionLimitError),
    Hub(LiveHubError),
    Catalog(CatalogError),
    Vod(VodError),
}

pub(in super::super) fn release_role<C, F>(
    hub: &LiveHub,
    registration: &mut Option<C>,
    fanout: &mut Option<F>,
    at_unix_ms: u64,
    release: impl FnOnce(&mut C, u64) -> Result<(), CatalogError>,
) -> Result<(), CatalogError> {
    let hub = hub.clone();
    let _transaction = hub.lock_roles();
    let result = registration.take().map_or(Ok(()), |mut registration| {
        let result = release(&mut registration, at_unix_ms);
        drop(registration);
        result
    });
    fanout.take();
    result
}

pub(in super::super) fn drop_role<C, F>(
    hub: &LiveHub,
    registration: &mut Option<C>,
    fanout: &mut Option<F>,
) {
    if registration.is_none() && fanout.is_none() {
        return;
    }
    let hub = hub.clone();
    let _transaction = hub.lock_roles();
    registration.take();
    fanout.take();
}
