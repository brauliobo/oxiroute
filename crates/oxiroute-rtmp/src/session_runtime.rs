use std::{collections::BTreeMap, sync::Arc};

use crate::{
    CatalogError, LiveHub, LiveHubError, RecorderDefinition, RtmpPushTarget, RtmpRecorderPolicy,
    RtmpRecorderStart, RtmpRegistry, SessionId, StreamKey,
    recording_runtime::{RecorderController, RecorderReaperHandle, RecorderReaperOwner},
    relay::RtmpRelayController,
};

use super::{
    RtmpSession,
    playback::PlaybackSession,
    publish::{PublishSession, PublisherOutputs},
};

#[derive(Clone)]
pub struct RtmpApplication {
    name: String,
    live: bool,
    idle_streams: bool,
    hub: Option<LiveHub>,
    push_targets: Arc<Vec<RtmpPushTarget>>,
    recorders: Arc<Vec<RtmpRecorderPolicy>>,
}

impl RtmpApplication {
    #[must_use]
    pub fn new(name: impl Into<String>, live: bool, idle_streams: bool) -> Self {
        Self {
            name: name.into(),
            live,
            idle_streams,
            hub: None,
            push_targets: Arc::new(Vec::new()),
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
        Self {
            name: name.into(),
            live,
            idle_streams,
            hub: None,
            push_targets: Arc::new(Vec::new()),
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
        Self {
            name: name.into(),
            live,
            idle_streams,
            hub: Some(hub),
            push_targets: Arc::new(push_targets.into_iter().collect()),
            recorders: Arc::new(recorders.into_iter().collect()),
        }
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

    fn hub(&self) -> Option<LiveHub> {
        self.hub.clone()
    }
}

#[derive(Clone)]
pub struct RtmpSessionPolicy {
    applications: Arc<BTreeMap<String, RtmpApplication>>,
    outbound_chunk_size: u32,
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

    fn application(&self, name: &str) -> Option<&RtmpApplication> {
        self.applications.get(name)
    }

    pub(super) const fn outbound_chunk_size(&self) -> u32 {
        self.outbound_chunk_size
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
    service_id: Arc<str>,
    registry: Arc<RtmpRegistry>,
    hub: LiveHub,
    policy: RtmpSessionPolicy,
    recorder_reaper: Option<RecorderReaperHandle>,
    recorder_reaper_owner: Option<Arc<RecorderReaperOwner>>,
}

impl RtmpServiceRuntime {
    #[must_use]
    pub fn new(
        service_id: impl Into<Arc<str>>,
        registry: Arc<RtmpRegistry>,
        hub: LiveHub,
        policy: RtmpSessionPolicy,
    ) -> Self {
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
        Self {
            service_id: service_id.into(),
            registry,
            hub,
            policy,
            recorder_reaper,
            recorder_reaper_owner,
        }
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
        RtmpSession::from_runtime(self.for_session())
    }

    pub(super) const fn outbound_chunk_size(&self) -> u32 {
        self.policy.outbound_chunk_size()
    }

    pub(super) fn application(&self, name: &str) -> Option<&RtmpApplication> {
        self.policy.application(name)
    }

    fn for_session(&self) -> Self {
        Self {
            service_id: Arc::clone(&self.service_id),
            registry: Arc::clone(&self.registry),
            hub: self.hub.clone(),
            policy: self.policy.clone(),
            recorder_reaper: self.recorder_reaper.clone(),
            recorder_reaper_owner: self.recorder_reaper_owner.clone(),
        }
    }

    pub(super) fn acquire_publisher_role(
        &self,
        key: StreamKey,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<PublishSession, PublisherRoleError> {
        let hub = self
            .application(&key.application)
            .and_then(RtmpApplication::hub)
            .unwrap_or_else(|| self.hub.clone());
        let _transaction = hub.lock_roles();
        let lease = hub
            .attach_publisher(key.clone())
            .map_err(PublisherRoleError::Hub)?;
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
            .map(|target| RtmpRelayController::start(target.expand(&key.name), target.config))
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
        debug_assert!(self.registry.has_publisher(&key));
        Ok(PublishSession::new(
            key,
            hub.clone(),
            lease,
            registration,
            Arc::clone(&self.registry),
            session_id,
            PublisherOutputs { recorders, relays },
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
        let hub = self
            .application(&key.application)
            .and_then(RtmpApplication::hub)
            .unwrap_or_else(|| self.hub.clone());
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
        ))
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

pub(super) enum SessionRole {
    Publisher(PublishSession),
    Playback(PlaybackSession),
}

impl SessionRole {
    pub(super) fn observe_at(&mut self, at_unix_ms: u64) {
        match self {
            Self::Publisher(publisher) => publisher.observe_at(at_unix_ms),
            Self::Playback(playback) => playback.observe_at(at_unix_ms),
        }
    }

    pub(super) fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        match self {
            Self::Publisher(publisher) => publisher.release(at_unix_ms),
            Self::Playback(playback) => playback.release(at_unix_ms),
        }
    }
}

#[derive(Debug)]
pub(super) enum PublisherRoleError {
    Hub(LiveHubError),
    Catalog(CatalogError),
}

#[derive(Debug)]
pub(super) enum PlaybackRoleError {
    NoPublisher,
    Hub(LiveHubError),
    Catalog(CatalogError),
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
    };

    use super::*;
    use crate::{LiveHubLimits, RtmpCapabilities};

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
}
