#[allow(clippy::wildcard_imports)]
use super::*;

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
    pub(crate) fn build_runtime_application(
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
