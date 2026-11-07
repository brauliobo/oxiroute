#[allow(clippy::wildcard_imports)]
use super::*;

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
    recording_retirement_started: Arc<AtomicBool>,
    recording_shutdown_initiated: Arc<Once>,
    #[cfg(test)]
    retirement_callback_count: Arc<std::sync::atomic::AtomicUsize>,
}

pub struct RtmpServicePreparation {
    service_id: Arc<str>,
    hub: LiveHub,
    policy: RtmpSessionPolicy,
    callbacks: RtmpCallbackPolicy,
    auto_push: Option<RtmpAutoPushConfig>,
}

impl RtmpServicePreparation {
    #[must_use]
    pub fn new(service_id: impl Into<Arc<str>>, hub: LiveHub, policy: RtmpSessionPolicy) -> Self {
        Self {
            service_id: service_id.into(),
            hub,
            policy,
            callbacks: RtmpCallbackPolicy::default(),
            auto_push: None,
        }
    }

    #[must_use]
    pub fn with_callbacks(mut self, callbacks: RtmpCallbackPolicy) -> Self {
        self.callbacks = callbacks;
        self
    }

    /// # Errors
    ///
    /// Returns an auto-push configuration error when the preparation cannot retain the policy.
    pub fn with_auto_push(
        mut self,
        config: RtmpAutoPushConfig,
    ) -> Result<Self, RtmpAutoPushConfigError> {
        config.validate_intrinsic()?;
        self.auto_push = config.enabled.then_some(config);
        Ok(self)
    }

    /// Starts recorder reaping and configured pull controllers exactly once by consuming the
    /// preparation. Auto-push transport remains lazy until the first publisher is admitted.
    ///
    /// # Errors
    ///
    /// Returns an auto-push configuration error without returning a partially started runtime.
    pub fn start(
        self,
        registry: Arc<RtmpRegistry>,
    ) -> Result<RtmpServiceRuntime, RtmpAutoPushConfigError> {
        if let Some(auto_push) = &self.auto_push {
            auto_push.validate_intrinsic()?;
        }
        let mut runtime =
            RtmpServiceRuntime::start(self.service_id, registry, self.hub, self.policy)
                .with_callbacks(self.callbacks);
        if let Some(auto_push) = self.auto_push {
            runtime = runtime.with_auto_push(auto_push)?;
        }
        Ok(runtime)
    }
}

/// Cheap, generation-independent ownership of one recorder shutdown lifecycle.
#[derive(Clone)]
pub struct RtmpRecorderLifecycle {
    control: RecorderShutdownControl,
    initiated: Arc<Once>,
    owner: Weak<RecorderReaperOwner>,
    reaper: RecorderReaperHandle,
    registry: Arc<RtmpRegistry>,
    shutdown: RtmpRecorderShutdown,
}

impl RtmpRecorderLifecycle {
    pub(crate) fn shutdown_handle(&self) -> RtmpRecorderShutdown {
        self.shutdown.clone()
    }

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
        self.initiated.call_once(|| {
            if let Some(owner) = self.owner.upgrade() {
                drop(owner.initiate_shutdown(deadline));
            }
            self.registry.initiate_recorder_shutdown(&self.reaper);
        });
        self.control.initiate_shutdown(deadline)
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
        Self::start(service_id, registry, hub, policy)
    }

    fn start(
        service_id: impl Into<Arc<str>>,
        registry: Arc<RtmpRegistry>,
        hub: LiveHub,
        policy: RtmpSessionPolicy,
    ) -> Self {
        #[cfg(test)]
        PREPARATION_STARTS.with(|count| count.set(count.get() + 1));
        let service_id = service_id.into();
        let max_recorders_per_stream = policy
            .applications
            .values()
            .map(|application| application.recorders.len())
            .max()
            .unwrap_or(0);
        let recorder_runtime = (max_recorders_per_stream > 0).then(|| {
            #[cfg(test)]
            REAPER_STARTS.with(|count| count.set(count.get() + 1));
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
                    #[cfg(test)]
                    PULL_STARTS.with(|count| count.set(count.get() + 1));
                    RtmpPullController::start(
                        Arc::clone(&service_id),
                        target,
                        Arc::clone(&registry),
                        application.hub().unwrap_or_else(|| hub.clone()),
                    )
                })
            })
            .collect();
        let runtime = Self {
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
            recording_retirement_started: Arc::new(AtomicBool::new(false)),
            recording_shutdown_initiated: Arc::new(Once::new()),
            #[cfg(test)]
            retirement_callback_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        runtime.install_recording_retirement();
        runtime
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
    /// Returns an auto-push configuration error before the coordinator is constructed.
    pub fn with_auto_push(
        mut self,
        config: RtmpAutoPushConfig,
    ) -> Result<Self, RtmpAutoPushConfigError> {
        config.validate_intrinsic()?;
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
    pub(crate) fn registry(&self) -> &Arc<RtmpRegistry> {
        &self.registry
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn hub(&self) -> LiveHub {
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

    #[cfg(test)]
    pub(crate) fn close_admission(&self) {
        self.close_service_admission();
        self.registry.close_admission();
    }

    pub(crate) fn admission_is_open(&self) -> bool {
        *self
            .admission_open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn close_service_admission(&self) {
        let mut admission_open = self
            .admission_open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*admission_open {
            return;
        }
        *admission_open = false;
        if let Some(auto_push) = &self.auto_push {
            auto_push.close();
        }
        for controller in self.pull_controllers.iter() {
            controller.deactivate();
        }
    }

    #[must_use]
    pub(crate) fn recorder_lifecycle(&self) -> Option<RtmpRecorderLifecycle> {
        let (Some(owner), Some(reaper)) = (&self.recorder_reaper_owner, &self.recorder_reaper)
        else {
            return None;
        };
        let lifecycle = RtmpRecorderLifecycle {
            control: reaper.shutdown_control(),
            initiated: Arc::clone(&self.recording_shutdown_initiated),
            owner: Arc::downgrade(owner),
            reaper: reaper.clone(),
            registry: Arc::clone(&self.registry),
            shutdown: owner.shutdown_handle(),
        };
        Some(lifecycle)
    }

    pub(crate) fn initiate_recorder_shutdown_scoped(
        &self,
        deadline: Instant,
    ) -> Option<RtmpRecorderShutdown> {
        self.close_service_admission();
        self.initiate_recorder_shutdown_after_admission_close(deadline)
    }

    fn initiate_recorder_shutdown_after_admission_close(
        &self,
        deadline: Instant,
    ) -> Option<RtmpRecorderShutdown> {
        let (Some(owner), Some(reaper)) = (&self.recorder_reaper_owner, &self.recorder_reaper)
        else {
            return None;
        };
        self.recording_shutdown_initiated.call_once(|| {
            drop(owner.initiate_shutdown(deadline));
            self.registry.initiate_recorder_shutdown(reaper);
        });
        Some(reaper.shutdown_control().initiate_shutdown(deadline))
    }

    fn install_recording_retirement(&self) {
        let Some(owner) = &self.recorder_reaper_owner else {
            return;
        };
        let shutdown = owner.shutdown_handle();
        if !self
            .recording_retirement_started
            .swap(true, Ordering::AcqRel)
        {
            let policy = self.policy.clone();
            #[cfg(test)]
            let callback_count = Arc::clone(&self.retirement_callback_count);
            shutdown.set_retirement(move || {
                for application in policy.applications.values() {
                    for recorder in application.recorder_policies() {
                        recorder.retire_store();
                    }
                }
                #[cfg(test)]
                callback_count.fetch_add(1, Ordering::AcqRel);
            });
        }
    }

    #[cfg(test)]
    pub(super) fn retirement_callback_count(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        Arc::clone(&self.retirement_callback_count)
    }

    #[must_use]
    pub fn auto_push_status(&self) -> RtmpAutoPushStatus {
        self.auto_push
            .as_ref()
            .map_or_else(RtmpAutoPushStatus::default, |auto_push| auto_push.status())
    }

    pub(in super::super) const fn outbound_chunk_size(&self) -> u32 {
        self.policy.outbound_chunk_size()
    }

    pub(in super::super) const fn inbound_limits(&self) -> RtmpSessionLimits {
        self.policy.inbound_limits()
    }

    pub(in super::super) fn application(&self, name: &str) -> Option<&RtmpApplication> {
        self.policy.application(name)
    }

    pub(in super::super) fn callbacks(&self) -> &RtmpCallbackPolicy {
        &self.callbacks
    }

    pub(in super::super) fn authorize(
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

    pub(in super::super) fn acquire_connection(
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
            recording_retirement_started: Arc::clone(&self.recording_retirement_started),
            recording_shutdown_initiated: Arc::clone(&self.recording_shutdown_initiated),
            #[cfg(test)]
            retirement_callback_count: Arc::clone(&self.retirement_callback_count),
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
    pub(in super::super) fn acquire_publisher_role(
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
            .map(|target| {
                RtmpRelayController::start(target.expand(&key.name), target.config.clone())
            })
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

    pub(in super::super) fn acquire_playback_role(
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

    pub(in super::super) fn acquire_vod_playback(
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
    pub(super) fn publisher_presence(&self, key: &StreamKey) -> (bool, bool) {
        let _transaction = self.hub.lock_roles();
        (
            self.hub.has_publisher(key),
            self.registry.has_publisher(key),
        )
    }
}
