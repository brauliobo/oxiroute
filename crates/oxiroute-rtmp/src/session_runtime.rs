use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{Arc, Mutex, Weak},
    time::Instant,
};

use crate::{
    CatalogError, LiveHub, LiveHubError, MediaApplication, PublisherLease, RecorderDefinition,
    RtmpAutoPushConfig, RtmpAutoPushError, RtmpAutoPushStatus, RtmpCallbackPolicy, RtmpPullTarget,
    RtmpPushTarget, RtmpRecorderPolicy, RtmpRecorderStart, RtmpRegistry, SessionId, StreamKey,
    VodApplication, VodError,
    auto_push::AutoPushCoordinator,
    exec::ExecProfile,
    exec_worker::ExecProfileSet,
    recording_runtime::{
        RecorderController, RecorderReaperHandle, RecorderReaperOwner, RecorderShutdownControl,
        RtmpRecorderShutdown,
    },
    relay::{RtmpPullController, RtmpRelayController},
};
use rml_rtmp::messages::Amf0Limits;

use super::{
    MAX_INBOUND_AMF0_CONTAINER_ENTRIES, MAX_INBOUND_AMF0_DEPTH, MAX_INBOUND_AMF0_STRING_BYTES,
    MAX_INBOUND_AMF0_VALUES, MAX_INBOUND_CHUNK_SIZE, MAX_INBOUND_MESSAGE_SIZE, RtmpSession,
    playback::PlaybackSession,
    publish::{PublishSession, PublisherOutputs},
    vod_playback::{VodPlaybackSession, VodPlaybackStart},
};

pub const RTMP_STALE_PUBLISHER_THRESHOLD_MS: u64 = 30_000;
const MAX_RTMP_TOKEN_BYTES: usize = 128;
const DEFAULT_RTMP_APPLICATION_CONNECTIONS: usize = 1_024;
const DEFAULT_RTMP_APPLICATION_PUBLISHERS: usize = 256;
const DEFAULT_RTMP_APPLICATION_VIEWERS: usize = 1_024;
const DEFAULT_RTMP_ACK_WINDOW_SIZE: u32 = 5_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpAccessAction {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RtmpNetwork {
    All,
    Cidr { address: IpAddr, prefix: u8 },
}

impl RtmpNetwork {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value == "all" {
            return Some(Self::All);
        }
        let (address, prefix) = value.split_once('/').map_or_else(
            || {
                let address = value.parse::<IpAddr>().ok()?;
                Some((address, if address.is_ipv4() { 32 } else { 128 }))
            },
            |(address, prefix)| Some((address.parse().ok()?, prefix.parse().ok()?)),
        )?;
        (prefix <= if address.is_ipv4() { 32 } else { 128 })
            .then_some(Self::Cidr { address, prefix })
    }

    fn matches(&self, peer: Option<IpAddr>) -> bool {
        match self {
            Self::All => true,
            Self::Cidr { address, prefix } => peer.is_some_and(|peer| match (address, peer) {
                (IpAddr::V4(address), IpAddr::V4(peer)) => {
                    masked(u32::from(*address), 32, *prefix) == masked(u32::from(peer), 32, *prefix)
                }
                (IpAddr::V6(address), IpAddr::V6(peer)) => {
                    masked(u128::from(*address), 128, *prefix)
                        == masked(u128::from(peer), 128, *prefix)
                }
                _ => false,
            }),
        }
    }
}

fn masked(value: impl Into<u128>, bits: u8, prefix: u8) -> u128 {
    let value = value.into();
    if prefix == 0 {
        0
    } else {
        value & (u128::MAX << u32::from(bits - prefix))
    }
}

#[derive(Clone, Debug)]
pub struct RtmpAccessRule {
    action: RtmpAccessAction,
    network: RtmpNetwork,
}

impl RtmpAccessRule {
    #[must_use]
    pub fn new(action: RtmpAccessAction, network: RtmpNetwork) -> Self {
        Self { action, network }
    }
}

#[derive(Clone)]
pub struct RtmpTokenPolicy {
    parameter: Arc<str>,
    secret: Arc<[u8]>,
}

impl std::fmt::Debug for RtmpTokenPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtmpTokenPolicy")
            .field("source", &"stream_query")
            .field("parameter", &self.parameter)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl RtmpTokenPolicy {
    #[must_use]
    pub fn stream_query(parameter: impl Into<Arc<str>>, secret: impl AsRef<[u8]>) -> Option<Self> {
        let parameter = parameter.into();
        let secret = secret.as_ref();
        let valid = (1..=32).contains(&parameter.len())
            && parameter
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            && !secret.is_empty()
            && secret.len() <= MAX_RTMP_TOKEN_BYTES
            && secret
                .iter()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'&' | b'=' | b'#' | b'?'));
        valid.then(|| Self {
            parameter,
            secret: Arc::from(secret),
        })
    }

    fn authorize(&self, query: Option<&str>) -> Result<(), RtmpAuthorizationError> {
        let Some(query) = query else {
            return Err(RtmpAuthorizationError::TokenMissing);
        };
        let mut value = None;
        for pair in query.split('&') {
            let Some((parameter, candidate)) = pair.split_once('=') else {
                return Err(RtmpAuthorizationError::QueryMalformed);
            };
            if parameter == self.parameter.as_ref() {
                if value.is_some() {
                    return Err(RtmpAuthorizationError::TokenRejected);
                }
                value = Some(candidate);
            }
        }
        let Some(value) = value else {
            return Err(RtmpAuthorizationError::TokenMissing);
        };
        if constant_time_eq(value.as_bytes(), &self.secret) {
            Ok(())
        } else {
            Err(RtmpAuthorizationError::TokenRejected)
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = u8::from(left.len() != right.len());
    for index in 0..MAX_RTMP_TOKEN_BYTES {
        let left = left.get(index).copied().unwrap_or_default();
        let right = right.get(index).copied().unwrap_or_default();
        difference |= left ^ right;
    }
    difference == 0
}

#[derive(Clone, Debug, Default)]
pub struct RtmpAccessPolicy {
    rules: Arc<Vec<RtmpAccessRule>>,
    token: Option<RtmpTokenPolicy>,
}

impl RtmpAccessPolicy {
    #[must_use]
    pub fn new(
        rules: impl IntoIterator<Item = RtmpAccessRule>,
        token: Option<RtmpTokenPolicy>,
    ) -> Self {
        Self {
            rules: Arc::new(rules.into_iter().collect()),
            token,
        }
    }

    fn authorize(
        &self,
        peer: Option<IpAddr>,
        query: Option<&str>,
    ) -> Result<(), RtmpAuthorizationError> {
        if !self.rules.is_empty() {
            let Some(rule) = self.rules.iter().find(|rule| rule.network.matches(peer)) else {
                return Err(RtmpAuthorizationError::NetworkDenied);
            };
            if rule.action == RtmpAccessAction::Deny {
                return Err(RtmpAuthorizationError::NetworkDenied);
            }
        }
        if let Some(token) = &self.token {
            token.authorize(query)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpSessionCeilings {
    pub max_connections: usize,
    pub max_publishers: usize,
    pub max_viewers: usize,
}

impl RtmpSessionCeilings {
    #[must_use]
    pub const fn new(max_connections: usize, max_publishers: usize, max_viewers: usize) -> Self {
        Self {
            max_connections,
            max_publishers,
            max_viewers,
        }
    }
}

impl Default for RtmpSessionCeilings {
    fn default() -> Self {
        Self::new(
            DEFAULT_RTMP_APPLICATION_CONNECTIONS,
            DEFAULT_RTMP_APPLICATION_PUBLISHERS,
            DEFAULT_RTMP_APPLICATION_VIEWERS,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RtmpAuthorizationError {
    NetworkDenied,
    TokenMissing,
    TokenRejected,
    QueryMalformed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SessionCounter {
    Connections,
    Publishers,
    Viewers,
}

#[derive(Clone, Copy, Debug, Default)]
struct SessionCounts {
    connections: usize,
    publishers: usize,
    viewers: usize,
}

struct ApplicationAdmission {
    limits: RtmpSessionCeilings,
    counts: Mutex<SessionCounts>,
}

impl ApplicationAdmission {
    fn new(limits: RtmpSessionCeilings) -> Self {
        Self {
            limits,
            counts: Mutex::new(SessionCounts::default()),
        }
    }

    fn acquire(
        self: &Arc<Self>,
        counter: SessionCounter,
    ) -> Result<ApplicationSessionLease, SessionLimitError> {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (current, maximum) = match counter {
            SessionCounter::Connections => (&mut counts.connections, self.limits.max_connections),
            SessionCounter::Publishers => (&mut counts.publishers, self.limits.max_publishers),
            SessionCounter::Viewers => (&mut counts.viewers, self.limits.max_viewers),
        };
        if *current >= maximum {
            return Err(SessionLimitError { counter, maximum });
        }
        *current += 1;
        Ok(ApplicationSessionLease {
            admission: Arc::clone(self),
            counter,
        })
    }
}

pub(super) struct ApplicationSessionLease {
    admission: Arc<ApplicationAdmission>,
    counter: SessionCounter,
}

impl Drop for ApplicationSessionLease {
    fn drop(&mut self) {
        let mut counts = self
            .admission
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = match self.counter {
            SessionCounter::Connections => &mut counts.connections,
            SessionCounter::Publishers => &mut counts.publishers,
            SessionCounter::Viewers => &mut counts.viewers,
        };
        *current = current.saturating_sub(1);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SessionLimitError {
    pub(super) counter: SessionCounter,
    pub(super) maximum: usize,
}

/// Runtime-only admission bounds for one RTMP session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpSessionLimits {
    pub max_inbound_chunk_size: u32,
    pub max_inbound_message_size: usize,
    pub window_ack_size: u32,
    pub max_amf0_depth: usize,
    pub max_amf0_container_entries: usize,
    pub max_amf0_values: usize,
    pub max_amf0_string_bytes: usize,
}

impl RtmpSessionLimits {
    /// Creates explicit inbound chunk, message, and AMF0 admission bounds.
    #[must_use]
    pub const fn new(
        max_inbound_chunk_size: u32,
        max_inbound_message_size: usize,
        max_amf0_depth: usize,
        max_amf0_container_entries: usize,
        max_amf0_values: usize,
        max_amf0_string_bytes: usize,
    ) -> Self {
        Self {
            max_inbound_chunk_size,
            max_inbound_message_size,
            window_ack_size: DEFAULT_RTMP_ACK_WINDOW_SIZE,
            max_amf0_depth,
            max_amf0_container_entries,
            max_amf0_values,
            max_amf0_string_bytes,
        }
    }

    #[must_use]
    pub const fn with_window_ack_size(mut self, window_ack_size: u32) -> Self {
        self.window_ack_size = window_ack_size;
        self
    }

    #[must_use]
    pub const fn with_max_inbound_message_size(mut self, max_inbound_message_size: usize) -> Self {
        self.max_inbound_message_size = max_inbound_message_size;
        self
    }

    pub(super) fn amf0_limits(self) -> Amf0Limits {
        Amf0Limits::new(
            self.max_amf0_depth,
            self.max_amf0_container_entries,
            self.max_amf0_values,
            self.max_amf0_string_bytes,
        )
    }
}

impl Default for RtmpSessionLimits {
    fn default() -> Self {
        Self::new(
            MAX_INBOUND_CHUNK_SIZE,
            MAX_INBOUND_MESSAGE_SIZE,
            MAX_INBOUND_AMF0_DEPTH,
            MAX_INBOUND_AMF0_CONTAINER_ENTRIES,
            MAX_INBOUND_AMF0_VALUES,
            MAX_INBOUND_AMF0_STRING_BYTES,
        )
    }
}

#[derive(Clone)]
pub struct RtmpApplication {
    name: String,
    live: bool,
    idle_streams: bool,
    publish_policy: RtmpAccessPolicy,
    play_policy: RtmpAccessPolicy,
    session_limits: RtmpSessionCeilings,
    admission: Arc<ApplicationAdmission>,
    hub: Option<LiveHub>,
    push_targets: Arc<Vec<RtmpPushTarget>>,
    pull_targets: Arc<Vec<RtmpPullTarget>>,
    callbacks: Arc<RtmpCallbackPolicy>,
    vod: Option<Arc<VodApplication>>,
    media: Option<Arc<MediaApplication>>,
    exec_profiles: Option<Arc<ExecProfileSet>>,
    recorders: Arc<Vec<RtmpRecorderPolicy>>,
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
    pub fn with_exec_profiles(
        mut self,
        profiles: impl IntoIterator<Item = ExecProfile>,
    ) -> Self {
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

    fn hub(&self) -> Option<LiveHub> {
        self.hub.clone()
    }

    fn policy(&self, operation: SessionOperation) -> &RtmpAccessPolicy {
        match operation {
            SessionOperation::Publish => &self.publish_policy,
            SessionOperation::Play => &self.play_policy,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum SessionOperation {
    Publish,
    Play,
}

#[derive(Clone)]
pub struct RtmpSessionPolicy {
    applications: Arc<BTreeMap<String, RtmpApplication>>,
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

    fn application(&self, name: &str) -> Option<&RtmpApplication> {
        self.applications.get(name)
    }

    pub(super) const fn outbound_chunk_size(&self) -> u32 {
        self.outbound_chunk_size
    }

    pub(super) const fn inbound_limits(&self) -> RtmpSessionLimits {
        self.inbound_limits
    }

    fn first_hub(&self) -> Option<LiveHub> {
        self.applications.values().find_map(RtmpApplication::hub)
    }
}

impl Default for RtmpSessionPolicy {
    fn default() -> Self {
        Self::new([])
    }
}

/// Shared construction context for every listener attached to one configured RTMP service.
#[derive(Clone)]
pub struct RtmpServiceRuntime {
    admission_open: Arc<Mutex<bool>>,
    service_id: Arc<str>,
    registry: Arc<RtmpRegistry>,
    hub: LiveHub,
    policy: RtmpSessionPolicy,
    default_admission: Arc<ApplicationAdmission>,
    recorder_reaper: Option<RecorderReaperHandle>,
    recorder_reaper_owner: Option<Arc<RecorderReaperOwner>>,
    pull_controllers: Arc<Vec<Arc<RtmpPullController>>>,
    callbacks: Arc<RtmpCallbackPolicy>,
    auto_push: Option<Arc<AutoPushCoordinator>>,
}

/// Cheap, generation-independent ownership of one recorder shutdown lifecycle.
#[derive(Clone)]
pub struct RtmpRecorderLifecycle {
    control: RecorderShutdownControl,
    owner: Weak<RecorderReaperOwner>,
    reaper: RecorderReaperHandle,
    registry: Arc<RtmpRegistry>,
    shutdown: RtmpRecorderShutdown,
}

impl RtmpRecorderLifecycle {
    #[must_use]
    pub fn is_same_lifecycle(&self, other: &Self) -> bool {
        self.shutdown.is_same_lifecycle(&other.shutdown)
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.shutdown.is_complete()
    }

    #[must_use]
    pub fn initiate_shutdown(&self, deadline: Instant) -> RtmpRecorderShutdown {
        if let Some(owner) = self.owner.upgrade() {
            drop(owner.initiate_shutdown(deadline));
        }
        let shutdown = self.control.initiate_shutdown(deadline);
        self.registry.initiate_recorder_shutdown(&self.reaper);
        shutdown
    }
}

impl RtmpServiceRuntime {
    #[must_use]
    pub fn new(
        service_id: impl Into<Arc<str>>,
        registry: Arc<RtmpRegistry>,
        hub: LiveHub,
        policy: RtmpSessionPolicy,
    ) -> Self {
        let service_id = service_id.into();
        let max_recorders_per_stream = policy
            .applications
            .values()
            .map(|application| application.recorders.len())
            .max()
            .unwrap_or(0);
        let recorder_runtime = (max_recorders_per_stream > 0).then(|| {
            let capacity = hub
                .limits()
                .max_streams
                .saturating_mul(max_recorders_per_stream);
            registry.create_recorder_reaper(capacity)
        });
        let (recorder_reaper_owner, recorder_reaper) =
            recorder_runtime.map_or((None, None), |(owner, handle)| (Some(owner), Some(handle)));
        let pull_controllers = policy
            .applications
            .values()
            .flat_map(|application| {
                application.pull_targets.iter().cloned().map(|target| {
                    RtmpPullController::start(
                        Arc::clone(&service_id),
                        target,
                        Arc::clone(&registry),
                        application.hub().unwrap_or_else(|| hub.clone()),
                    )
                })
            })
            .collect();
        Self {
            admission_open: Arc::new(Mutex::new(true)),
            service_id,
            registry,
            hub,
            policy,
            default_admission: Arc::new(ApplicationAdmission::new(RtmpSessionCeilings::default())),
            recorder_reaper,
            recorder_reaper_owner,
            pull_controllers: Arc::new(pull_controllers),
            callbacks: Arc::new(RtmpCallbackPolicy::default()),
            auto_push: None,
        }
    }

    #[must_use]
    pub fn with_callbacks(mut self, callbacks: RtmpCallbackPolicy) -> Self {
        self.callbacks = Arc::new(callbacks);
        self
    }

    /// Adds the configured auto-push coordinator to the runtime.
    ///
    /// # Errors
    ///
    /// Returns an auto-push configuration error when the coordinator cannot be constructed.
    pub fn with_auto_push(mut self, config: RtmpAutoPushConfig) -> Result<Self, RtmpAutoPushError> {
        if config.enabled {
            let application_hubs = self
                .policy
                .applications
                .iter()
                .map(|(name, application)| {
                    (
                        name.clone(),
                        application.hub().unwrap_or_else(|| self.hub.clone()),
                    )
                })
                .collect();
            self.auto_push = Some(Arc::new(AutoPushCoordinator::new(
                config,
                Arc::clone(&self.service_id),
                Arc::clone(&self.registry),
                self.hub.clone(),
                application_hubs,
            )));
        }
        Ok(self)
    }

    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    #[must_use]
    pub fn registry(&self) -> &Arc<RtmpRegistry> {
        &self.registry
    }

    #[must_use]
    pub fn hub(&self) -> LiveHub {
        self.policy.first_hub().unwrap_or_else(|| self.hub.clone())
    }

    #[must_use]
    pub fn session(&self) -> RtmpSession {
        RtmpSession::from_runtime(self.for_session(), None)
    }

    #[must_use]
    pub fn session_with_peer_addr(&self, peer_addr: Option<IpAddr>) -> RtmpSession {
        RtmpSession::from_runtime(self.for_session(), peer_addr)
    }

    pub fn close_admission(&self) {
        let mut admission_open = self
            .admission_open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*admission_open {
            return;
        }
        *admission_open = false;
        self.registry.close_admission();
        if let Some(auto_push) = &self.auto_push {
            auto_push.close();
        }
        for controller in self.pull_controllers.iter() {
            controller.deactivate();
        }
    }

    #[must_use]
    pub fn recorder_lifecycle(&self) -> Option<RtmpRecorderLifecycle> {
        let (Some(owner), Some(reaper)) = (&self.recorder_reaper_owner, &self.recorder_reaper)
        else {
            return None;
        };
        Some(RtmpRecorderLifecycle {
            control: reaper.shutdown_control(),
            owner: Arc::downgrade(owner),
            reaper: reaper.clone(),
            registry: Arc::clone(&self.registry),
            shutdown: owner.shutdown_handle(),
        })
    }

    #[must_use]
    pub fn initiate_recorder_shutdown(&self, deadline: Instant) -> Option<RtmpRecorderShutdown> {
        self.close_admission();
        let (Some(owner), Some(reaper)) = (&self.recorder_reaper_owner, &self.recorder_reaper)
        else {
            return None;
        };
        let shutdown = owner.initiate_shutdown(deadline);
        self.registry.initiate_recorder_shutdown(reaper);
        Some(shutdown)
    }

    #[must_use]
    pub fn auto_push_status(&self) -> RtmpAutoPushStatus {
        self.auto_push
            .as_ref()
            .map_or_else(RtmpAutoPushStatus::default, |auto_push| auto_push.status())
    }

    pub(super) const fn outbound_chunk_size(&self) -> u32 {
        self.policy.outbound_chunk_size()
    }

    pub(super) const fn inbound_limits(&self) -> RtmpSessionLimits {
        self.policy.inbound_limits()
    }

    pub(super) fn application(&self, name: &str) -> Option<&RtmpApplication> {
        self.policy.application(name)
    }

    pub(super) fn callbacks(&self) -> &RtmpCallbackPolicy {
        &self.callbacks
    }

    pub(super) fn authorize(
        &self,
        application: &str,
        operation: SessionOperation,
        peer_addr: Option<IpAddr>,
        query: Option<&str>,
    ) -> Result<(), RtmpAuthorizationError> {
        self.application(application)
            .expect("authorization follows application lookup")
            .policy(operation)
            .authorize(peer_addr, query)
    }

    pub(super) fn acquire_connection(
        &self,
        application: &str,
    ) -> Result<ApplicationSessionLease, SessionLimitError> {
        self.admission(application)
            .acquire(SessionCounter::Connections)
    }

    fn admission(&self, application: &str) -> &Arc<ApplicationAdmission> {
        self.application(application)
            .map_or(&self.default_admission, |application| {
                &application.admission
            })
    }

    fn for_session(&self) -> Self {
        Self {
            admission_open: Arc::clone(&self.admission_open),
            service_id: Arc::clone(&self.service_id),
            registry: Arc::clone(&self.registry),
            hub: self.hub.clone(),
            policy: self.policy.clone(),
            default_admission: Arc::clone(&self.default_admission),
            recorder_reaper: self.recorder_reaper.clone(),
            recorder_reaper_owner: self.recorder_reaper_owner.clone(),
            pull_controllers: Arc::clone(&self.pull_controllers),
            callbacks: Arc::clone(&self.callbacks),
            auto_push: self.auto_push.clone(),
        }
    }

    fn acquire_publisher_lease(
        &self,
        hub: &LiveHub,
        key: &StreamKey,
        at_unix_ms: u64,
    ) -> Result<PublisherLease, PublisherRoleError> {
        match hub.attach_publisher(key.clone()) {
            Ok(lease) => Ok(lease),
            Err(error @ LiveHubError::PublisherAlreadyAttached { .. }) => {
                let Some(owner) = self.registry.stale_publisher_owner(
                    key,
                    at_unix_ms,
                    RTMP_STALE_PUBLISHER_THRESHOLD_MS,
                ) else {
                    return Err(PublisherRoleError::Hub(error));
                };
                let lease = hub
                    .replace_publisher(key.clone())
                    .map_err(PublisherRoleError::Hub)?;
                let shutdown = match self.registry.detach_expected_publisher(owner, at_unix_ms) {
                    Ok(shutdown) => shutdown,
                    Err(error) => {
                        drop(lease);
                        return Err(PublisherRoleError::Catalog(error));
                    }
                };
                shutdown.shutdown(at_unix_ms);
                Ok(lease)
            }
            Err(error) => Err(PublisherRoleError::Hub(error)),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn acquire_publisher_role(
        &self,
        key: StreamKey,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<PublishSession, PublisherRoleError> {
        let admission_open = self
            .admission_open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*admission_open {
            return Err(PublisherRoleError::AdmissionClosed);
        }
        let hub = self
            .application(&key.application)
            .and_then(RtmpApplication::hub)
            .unwrap_or_else(|| self.hub.clone());
        let media = self
            .application(&key.application)
            .and_then(RtmpApplication::media);
        let exec_profiles = self
            .application(&key.application)
            .and_then(RtmpApplication::exec_profiles);
        let auto_push = self.auto_push.clone();
        let role_lease = self
            .admission(&key.application)
            .acquire(SessionCounter::Publishers)
            .map_err(PublisherRoleError::SessionLimit)?;
        if let Some(auto_push) = auto_push.as_ref() {
            auto_push
                .ensure_started()
                .map_err(PublisherRoleError::AutoPush)?;
        }
        let transaction = hub.lock_roles();
        let lease = self.acquire_publisher_lease(&hub, &key, at_unix_ms)?;
        // Media output is best-effort; storage or worker failures must not reject RTMP publish.
        let media_publisher = media
            .as_ref()
            .and_then(|media| media.attach(&key, lease.incarnation()).ok())
            .flatten();
        let policies = self
            .application(&key.application)
            .map_or(&[][..], RtmpApplication::recorder_policies);
        let stream_name = Arc::<[u8]>::from(key.name.as_bytes());
        let controllers: Vec<_> = policies
            .iter()
            .cloned()
            .map(|policy| {
                Arc::new(RecorderController::new(
                    policy,
                    Arc::clone(&stream_name),
                    self.recorder_reaper
                        .as_ref()
                        .expect("recorder policies initialize a reaper")
                        .clone(),
                    at_unix_ms,
                ))
            })
            .collect();
        let relays: Vec<_> = self
            .application(&key.application)
            .map_or(&[][..], RtmpApplication::push_targets)
            .iter()
            .map(|target| RtmpRelayController::start(target.expand(&key.name), target.config.clone()))
            .collect();
        let definitions = policies
            .iter()
            .zip(&controllers)
            .map(|(policy, controller)| {
                let definition = match policy.start() {
                    RtmpRecorderStart::Continuous => {
                        RecorderDefinition::automatic(Some(policy.name().to_owned()))
                    }
                    RtmpRecorderStart::Manual => {
                        RecorderDefinition::manual(Some(policy.name().to_owned()))
                    }
                };
                (definition, Arc::clone(controller))
            })
            .collect();
        let registration = match self.registry.register_managed_publisher(
            key.clone(),
            session_id,
            definitions,
            relays.clone(),
            at_unix_ms,
        ) {
            Ok(registration) => registration,
            Err(error) => {
                drop(lease);
                return Err(PublisherRoleError::Catalog(error));
            }
        };
        let auto_push_publisher = match auto_push.as_ref() {
            Some(auto_push) => match auto_push.source(key.clone(), session_id, lease.incarnation())
            {
                Ok(publisher) => publisher,
                Err(error) => {
                    drop(registration);
                    drop(lease);
                    return Err(PublisherRoleError::AutoPush(error));
                }
            },
            None => None,
        };
        let recorders: Vec<_> = registration
            .recorder_ids()
            .iter()
            .copied()
            .zip(controllers)
            .collect();
        for ((recorder_id, _), policy) in recorders.iter().zip(policies) {
            if policy.start() == RtmpRecorderStart::Continuous {
                self.registry.start_continuous_recording(
                    registration.stream_id(),
                    session_id,
                    *recorder_id,
                    at_unix_ms,
                );
            }
        }
        drop(transaction);
        let exec_workers = exec_profiles.as_ref().map_or_else(Vec::new, |profiles| {
            profiles.start_publisher(&key.server_id, &key, session_id)
        });
        debug_assert!(self.registry.has_publisher(&key));
        Ok(PublishSession::new(
            key,
            hub.clone(),
            lease,
            registration,
            Arc::clone(&self.registry),
            session_id,
            PublisherOutputs {
                recorders,
                relays,
                media: media_publisher,
                session_lease: role_lease,
                exec_profiles,
                exec_workers,
                auto_push: auto_push_publisher,
            },
        ))
    }

    pub(super) fn acquire_playback_role(
        &self,
        key: StreamKey,
        session_id: SessionId,
        protocol_stream_id: u32,
        idle_streams: bool,
        at_unix_ms: u64,
    ) -> Result<PlaybackSession, PlaybackRoleError> {
        let admission_open = self
            .admission_open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*admission_open {
            return Err(PlaybackRoleError::AdmissionClosed);
        }
        let hub = self
            .application(&key.application)
            .and_then(RtmpApplication::hub)
            .unwrap_or_else(|| self.hub.clone());
        let role_lease = self
            .admission(&key.application)
            .acquire(SessionCounter::Viewers)
            .map_err(PlaybackRoleError::SessionLimit)?;
        let _transaction = hub.lock_roles();
        if !idle_streams && !hub.has_publisher(&key) {
            return Err(PlaybackRoleError::NoPublisher);
        }
        let subscription = hub.subscribe(key.clone()).map_err(PlaybackRoleError::Hub)?;
        let registration =
            match self
                .registry
                .register_subscriber(key.clone(), session_id, at_unix_ms)
            {
                Ok(registration) => registration,
                Err(error) => {
                    drop(subscription);
                    return Err(PlaybackRoleError::Catalog(error));
                }
            };
        Ok(PlaybackSession::new(
            key,
            hub.clone(),
            protocol_stream_id,
            subscription,
            registration,
            role_lease,
        ))
    }

    pub(super) fn acquire_vod_playback(
        &self,
        application: &str,
        source: &str,
        path: &str,
        stream_name: String,
        protocol_stream_id: u32,
    ) -> Result<VodPlaybackSession, PlaybackRoleError> {
        let admission_open = self
            .admission_open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*admission_open {
            return Err(PlaybackRoleError::AdmissionClosed);
        }
        let application_policy = self
            .application(application)
            .ok_or(PlaybackRoleError::NoPublisher)?;
        let vod = application_policy
            .vod()
            .ok_or(PlaybackRoleError::NoPublisher)?;
        vod.validate_request(source, path)
            .map_err(PlaybackRoleError::Vod)?;
        let session_lease = self
            .admission(application)
            .acquire(SessionCounter::Viewers)
            .map_err(PlaybackRoleError::SessionLimit)?;
        let vod_lease = vod.reserve().map_err(PlaybackRoleError::Vod)?;
        Ok(VodPlaybackSession::start(VodPlaybackStart {
            application: application.to_owned(),
            stream_name,
            protocol_stream_id,
            application_source: vod,
            source: source.to_owned(),
            path: path.to_owned(),
            vod_lease,
            session_lease,
        }))
    }

    #[cfg(test)]
    fn publisher_presence(&self, key: &StreamKey) -> (bool, bool) {
        let _transaction = self.hub.lock_roles();
        (
            self.hub.has_publisher(key),
            self.registry.has_publisher(key),
        )
    }
}

#[allow(clippy::large_enum_variant)]
pub(super) enum SessionRole {
    Publisher(PublishSession),
    Playback(PlaybackSession),
    VodPlayback(VodPlaybackSession),
}

impl SessionRole {
    pub(super) fn identity(&self) -> (&str, &str) {
        match self {
            Self::Publisher(publisher) => (publisher.application(), publisher.stream_name()),
            Self::Playback(playback) => (playback.application(), playback.stream_name()),
            Self::VodPlayback(playback) => (playback.application(), playback.stream_name()),
        }
    }

    pub(super) fn observe_at(&mut self, at_unix_ms: u64) {
        match self {
            Self::Publisher(publisher) => publisher.observe_at(at_unix_ms),
            Self::Playback(playback) => playback.observe_at(at_unix_ms),
            Self::VodPlayback(_) => {}
        }
    }

    pub(super) fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
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
pub(super) enum PublisherRoleError {
    AdmissionClosed,
    SessionLimit(SessionLimitError),
    Hub(LiveHubError),
    Catalog(CatalogError),
    AutoPush(RtmpAutoPushError),
}

#[derive(Debug)]
pub(super) enum PlaybackRoleError {
    AdmissionClosed,
    NoPublisher,
    SessionLimit(SessionLimitError),
    Hub(LiveHubError),
    Catalog(CatalogError),
    Vod(VodError),
}

pub(super) fn release_role<C, F>(
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

pub(super) fn drop_role<C, F>(hub: &LiveHub, registration: &mut Option<C>, fanout: &mut Option<F>) {
    if registration.is_none() && fanout.is_none() {
        return;
    }
    let hub = hub.clone();
    let _transaction = hub.lock_roles();
    registration.take();
    fanout.take();
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    };

    use super::*;
    use crate::{
        HlsFragmentNaming, HlsOutputConfig, LiveHubLimits, MediaApplication, MediaEvent,
        MediaStore, MediaStoreLimits, RtmpCapabilities,
    };

    #[test]
    fn publisher_reconnect_never_exposes_split_hub_and_catalog_ownership() {
        let runtime = RtmpServiceRuntime::new(
            "live",
            Arc::new(RtmpRegistry::new(RtmpCapabilities {
                live_ingest: true,
                manual_recording: false,
            })),
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::default(),
        );
        let key = StreamKey::new("live", "broadcast", "camera");
        let barrier = Arc::new(Barrier::new(2));

        let writer_runtime = runtime.clone();
        let writer_key = key.clone();
        let writer_barrier = Arc::clone(&barrier);
        let writer = thread::spawn(move || {
            writer_barrier.wait();
            for at_unix_ms in 1..=1_000 {
                let role = writer_runtime
                    .acquire_publisher_role(writer_key.clone(), SessionId::new(), at_unix_ms)
                    .expect("publisher role");
                assert_eq!(writer_runtime.publisher_presence(&writer_key), (true, true));
                drop(role);
                assert_eq!(
                    writer_runtime.publisher_presence(&writer_key),
                    (false, false)
                );
            }
        });

        barrier.wait();
        for _ in 0..2_000 {
            let (hub, catalog) = runtime.publisher_presence(&key);
            assert_eq!(hub, catalog);
        }
        writer.join().expect("publisher reconnect thread");
        assert_eq!(runtime.publisher_presence(&key), (false, false));
    }

    #[test]
    fn concurrent_stale_takeover_never_exposes_split_hub_and_catalog_ownership() {
        let runtime = RtmpServiceRuntime::new(
            "live",
            Arc::new(RtmpRegistry::new(RtmpCapabilities {
                live_ingest: true,
                manual_recording: false,
            })),
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::default(),
        );
        let key = StreamKey::new("live", "broadcast", "camera");
        let incumbent = runtime
            .acquire_publisher_role(key.clone(), SessionId::new(), 1)
            .expect("incumbent publisher");
        let barrier = Arc::new(Barrier::new(3));

        let first_runtime = runtime.clone();
        let first_key = key.clone();
        let first_barrier = Arc::clone(&barrier);
        let first = thread::spawn(move || {
            first_barrier.wait();
            first_runtime.acquire_publisher_role(
                first_key,
                SessionId::new(),
                1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS,
            )
        });
        let second_runtime = runtime.clone();
        let second_key = key.clone();
        let second_barrier = Arc::clone(&barrier);
        let second = thread::spawn(move || {
            second_barrier.wait();
            second_runtime.acquire_publisher_role(
                second_key,
                SessionId::new(),
                1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS,
            )
        });

        barrier.wait();
        for _ in 0..2_000 {
            let presence = runtime.publisher_presence(&key);
            assert_eq!(presence.0, presence.1);
        }

        let first_result = first.join().expect("first takeover thread");
        let second_result = second.join().expect("second takeover thread");
        let ((Ok(winner), Err(loser)) | (Err(loser), Ok(winner))) = (first_result, second_result)
        else {
            panic!("concurrent takeover did not produce one winner and one rejection")
        };
        assert!(matches!(
            loser,
            PublisherRoleError::Hub(LiveHubError::PublisherAlreadyAttached { .. })
        ));
        assert_eq!(runtime.publisher_presence(&key), (true, true));

        drop(incumbent);
        assert_eq!(runtime.publisher_presence(&key), (true, true));
        drop(winner);
        assert_eq!(runtime.publisher_presence(&key), (false, false));
    }

    #[test]
    fn takeover_expires_the_old_hub_lease_before_expected_catalog_detach() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        }));
        let hub = LiveHub::new(LiveHubLimits::default());
        let key = StreamKey::new("live", "broadcast", "camera");
        let old_session_id = SessionId::new();
        let old_lease = hub
            .attach_publisher(key.clone())
            .expect("old hub publisher");
        registry
            .attach_publisher(key.clone(), old_session_id, Vec::new(), 1)
            .expect("old catalog publisher");
        let owner = registry
            .stale_publisher_owner(
                &key,
                1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS,
                RTMP_STALE_PUBLISHER_THRESHOLD_MS,
            )
            .expect("stale catalog owner");

        let new_lease = hub
            .replace_publisher(key.clone())
            .expect("replacement hub publisher");
        let event =
            MediaEvent::audio(0, Arc::<[u8]>::from(&[0xaf, 0x01, 0x11][..])).expect("audio event");
        assert!(matches!(
            old_lease.publish(event.clone()),
            Err(LiveHubError::PublisherExpired { .. })
        ));

        let shutdown = registry
            .detach_expected_publisher(owner, 1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS)
            .expect("expected catalog detach");
        shutdown.shutdown(1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS);
        new_lease.publish(event).expect("replacement media");
        drop(old_lease);
        assert!(hub.has_publisher(&key));
    }

    #[test]
    fn existing_session_cannot_register_after_runtime_admission_closes() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        }));
        let runtime = RtmpServiceRuntime::new(
            "live",
            Arc::clone(&registry),
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::default(),
        );
        let admitted_session = runtime.clone();
        let key = StreamKey::new("live", "broadcast", "camera");

        runtime.close_admission();

        assert!(matches!(
            admitted_session.acquire_publisher_role(key.clone(), SessionId::new(), 1),
            Err(PublisherRoleError::AdmissionClosed)
        ));
        assert!(matches!(
            registry.register_publisher(key, SessionId::new(), Vec::new(), 1),
            Err(CatalogError::AdmissionClosed)
        ));
        assert!(registry.snapshot().streams.is_empty());
    }

    #[test]
    fn access_policy_uses_the_first_matching_rule_and_requires_the_configured_token() {
        let policy = RtmpAccessPolicy::new(
            [
                RtmpAccessRule::new(
                    RtmpAccessAction::Deny,
                    RtmpNetwork::parse("192.0.2.0/24").expect("valid network"),
                ),
                RtmpAccessRule::new(RtmpAccessAction::Allow, RtmpNetwork::All),
            ],
            Some(RtmpTokenPolicy::stream_query("token", "secret").expect("valid token policy")),
        );

        assert_eq!(
            policy.authorize(
                Some("192.0.2.10".parse().expect("valid peer")),
                Some("token=secret")
            ),
            Err(RtmpAuthorizationError::NetworkDenied)
        );
        assert_eq!(
            policy.authorize(
                Some("198.51.100.10".parse().expect("valid peer")),
                Some("token=wrong")
            ),
            Err(RtmpAuthorizationError::TokenRejected)
        );
        assert_eq!(
            policy.authorize(
                Some("198.51.100.10".parse().expect("valid peer")),
                Some("token=secret")
            ),
            Ok(())
        );
    }

    #[test]
    fn application_session_ceilings_are_shared_and_released() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        }));
        let application = RtmpApplication::new("broadcast", true, true).with_authorization(
            RtmpAccessPolicy::default(),
            RtmpAccessPolicy::default(),
            RtmpSessionCeilings::new(1, 1, 1),
        );
        let runtime = RtmpServiceRuntime::new(
            "live",
            registry,
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::new([application]),
        );

        let connection = runtime
            .acquire_connection("broadcast")
            .expect("first connection lease");
        assert!(matches!(
            runtime.acquire_connection("broadcast"),
            Err(SessionLimitError {
                counter: SessionCounter::Connections,
                ..
            })
        ));
        drop(connection);

        let key = StreamKey::new("live", "broadcast", "camera");
        let publisher = runtime
            .acquire_publisher_role(key.clone(), SessionId::new(), 1)
            .expect("first publisher lease");
        assert!(matches!(
            runtime.acquire_publisher_role(key.clone(), SessionId::new(), 2),
            Err(PublisherRoleError::SessionLimit(SessionLimitError {
                counter: SessionCounter::Publishers,
                ..
            }))
        ));
        drop(publisher);

        let viewer = runtime
            .acquire_playback_role(key.clone(), SessionId::new(), 1, true, 3)
            .expect("first viewer lease");
        assert!(matches!(
            runtime.acquire_playback_role(key, SessionId::new(), 1, true, 4),
            Err(PlaybackRoleError::SessionLimit(SessionLimitError {
                counter: SessionCounter::Viewers,
                ..
            }))
        ));
        drop(viewer);
    }

    #[test]
    fn media_store_limit_does_not_reject_publisher() {
        let directory = tempfile::tempdir().expect("temporary media directory");
        let store = Arc::new(
            MediaStore::open(
                directory.path(),
                MediaStoreLimits {
                    max_bytes: 1024 * 1024,
                    max_files: 16,
                    max_active_streams: 1,
                    max_file_bytes: 1024 * 1024,
                },
            )
            .expect("media store"),
        );
        let media = Arc::new(MediaApplication::new(Some(Arc::new(HlsOutputConfig {
            store,
            segment_duration: Duration::from_secs(1),
            max_segment_duration: Duration::from_secs(2),
            playlist_length: Duration::from_secs(6),
            naming: HlsFragmentNaming::Sequential,
            nested: false,
            cleanup: true,
            variants: Vec::new(),
            keys: None,
            max_segment_bytes: 1024 * 1024,
            max_queue_messages: 8,
        }))));
        let application = RtmpApplication::new("broadcast", true, true).with_media(Some(media));
        let runtime = RtmpServiceRuntime::new(
            "live",
            Arc::new(RtmpRegistry::new(RtmpCapabilities {
                live_ingest: true,
                manual_recording: false,
            })),
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::new([application]),
        );

        let first_key = StreamKey::new("live", "broadcast", "first");
        let first = runtime
            .acquire_publisher_role(first_key, SessionId::new(), 1)
            .expect("first publisher");
        let second_key = StreamKey::new("live", "broadcast", "second");
        let second = runtime
            .acquire_publisher_role(second_key.clone(), SessionId::new(), 2)
            .expect("media failure must not reject publisher");

        assert_eq!(runtime.publisher_presence(&second_key), (true, true));
        drop(second);
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn auto_push_transport_starts_only_when_a_publisher_is_admitted() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("auto-push socket directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("secure auto-push socket directory");
        let runtime = RtmpServiceRuntime::new(
            "live",
            Arc::new(RtmpRegistry::new(RtmpCapabilities {
                live_ingest: true,
                manual_recording: false,
            })),
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::default(),
        )
        .with_auto_push(RtmpAutoPushConfig {
            enabled: true,
            socket_dir: directory.path().to_path_buf(),
            secret_file: None,
            reconnect_interval: Duration::from_millis(100),
            connect_timeout: Duration::from_millis(500),
            handshake_timeout: Duration::from_millis(500),
            max_peers: 2,
            max_queue_messages: 16,
            max_queue_bytes: 1024 * 1024,
            max_streams: 2,
        })
        .expect("auto-push runtime");

        assert!(!runtime.auto_push_status().started);
        let role = runtime
            .acquire_publisher_role(
                StreamKey::new("live", "broadcast", "camera"),
                SessionId::new(),
                1,
            )
            .expect("publisher role");
        assert!(runtime.auto_push_status().started);
        assert_eq!(runtime.auto_push_status().source_streams, 1);
        drop(role);
        assert_eq!(runtime.auto_push_status().source_streams, 0);
        runtime.close_admission();
    }
}
