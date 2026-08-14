use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::IpAddr,
    sync::Arc,
    time::Instant,
};

use crate::{
    CatalogError, LiveHub, LiveHubLimits, MediaApplication, MediaCatalog, RecorderId,
    RecorderSnapshot, RtmpAutoPushStatus, RtmpCallbackEventPlan, RtmpCallbackPlan,
    RtmpCallbackPolicy, RtmpCapabilities, RtmpCatalogSnapshot, RtmpClientOptions,
    RtmpClientSnapshot, RtmpCredential, RtmpMediaPlan, RtmpMediaStoreRegistry, RtmpPrepareContext,
    RtmpPrepareMode, RtmpRecorderLifecycle, RtmpRecorderStoreRegistry, RtmpRegistry,
    RtmpServicePlan, RtmpServicePreparation, RtmpServiceRuntime, RtmpSession,
    RtmpSessionControlAction, RtmpSessionControlError, RtmpSessionControlOutcome,
    RtmpSessionPolicy, SessionId, StreamId, VodApplication, VodCatalog,
};

#[cfg(test)]
use std::sync::Mutex;

/// Opaque access to one started RTMP service runtime.
#[derive(Clone)]
pub struct RtmpServiceHandle {
    runtime: Arc<RtmpServiceRuntime>,
}

impl RtmpServiceHandle {
    fn new(runtime: RtmpServiceRuntime) -> Self {
        Self {
            runtime: Arc::new(runtime),
        }
    }

    /// Returns the stable configured service identifier.
    #[must_use]
    pub fn service_id(&self) -> &str {
        self.runtime.service_id()
    }

    /// Creates an incremental RTMP session owned by this service.
    #[must_use]
    pub fn session(&self) -> RtmpSession {
        self.runtime.session()
    }

    /// Creates an incremental RTMP session with the peer address retained for policy and status.
    #[must_use]
    pub fn session_with_peer_addr(&self, peer_addr: Option<IpAddr>) -> RtmpSession {
        self.runtime.session_with_peer_addr(peer_addr)
    }

    /// Returns the current aggregate auto-push status for this service.
    #[must_use]
    pub fn auto_push_status(&self) -> RtmpAutoPushStatus {
        self.runtime.auto_push_status()
    }
}

/// Opaque control-plane access to the catalogs and session controls of one RTMP runtime set.
#[derive(Clone)]
pub struct RtmpControlHandle {
    media_catalog: Arc<MediaCatalog>,
    registry: Arc<RtmpRegistry>,
    services: Arc<Vec<RtmpServiceHandle>>,
    service_ids: Option<Arc<BTreeSet<String>>>,
    vod_catalog: Arc<VodCatalog>,
}

impl RtmpControlHandle {
    fn new(
        registry: Arc<RtmpRegistry>,
        services: Vec<RtmpServiceHandle>,
        vod_catalog: Arc<VodCatalog>,
        media_catalog: Arc<MediaCatalog>,
    ) -> Self {
        let service_ids = services
            .iter()
            .map(|service| service.service_id().to_owned())
            .collect();
        Self {
            media_catalog,
            registry,
            services: Arc::new(services),
            service_ids: Some(Arc::new(service_ids)),
            vod_catalog,
        }
    }

    /// Returns the VOD catalog built from this runtime set's prepared applications.
    #[must_use]
    pub fn vod_catalog(&self) -> Arc<VodCatalog> {
        Arc::clone(&self.vod_catalog)
    }

    /// Returns the media catalog built from this runtime set's prepared applications.
    #[must_use]
    pub fn media_catalog(&self) -> Arc<MediaCatalog> {
        Arc::clone(&self.media_catalog)
    }

    /// Returns the aggregate auto-push status for this runtime set.
    #[must_use]
    pub fn auto_push_status(&self) -> RtmpAutoPushStatus {
        self.services
            .iter()
            .fold(RtmpAutoPushStatus::default(), |mut total, service| {
                let status = service.auto_push_status();
                total.enabled |= status.enabled;
                total.started |= status.started;
                total.peers = total.peers.saturating_add(status.peers);
                total.source_streams = total.source_streams.saturating_add(status.source_streams);
                total.remote_streams = total.remote_streams.saturating_add(status.remote_streams);
                total.frames_sent = total.frames_sent.saturating_add(status.frames_sent);
                total.frames_received =
                    total.frames_received.saturating_add(status.frames_received);
                total.frames_dropped = total.frames_dropped.saturating_add(status.frames_dropped);
                total.authentication_failures = total
                    .authentication_failures
                    .saturating_add(status.authentication_failures);
                total.reconnects = total.reconnects.saturating_add(status.reconnects);
                total.queue_messages = total.queue_messages.saturating_add(status.queue_messages);
                total.queue_bytes = total.queue_bytes.saturating_add(status.queue_bytes);
                total.last_failure = total.last_failure.or(status.last_failure);
                total
            })
    }

    /// Returns the current immutable stream catalog snapshot.
    #[must_use]
    pub fn catalog_snapshot(&self) -> Arc<RtmpCatalogSnapshot> {
        let registry_snapshot = self.registry.snapshot();
        let mut snapshot = (*registry_snapshot).clone();
        if let Some(service_ids) = &self.service_ids {
            snapshot
                .streams
                .retain(|stream| service_ids.contains(&stream.key.server_id));
        }
        Arc::new(snapshot)
    }

    /// Returns bounded snapshots of currently registered client sessions.
    #[must_use]
    pub fn session_snapshots(&self) -> Vec<RtmpClientSnapshot> {
        self.registry
            .session_snapshots()
            .into_iter()
            .filter(|session| {
                self.service_ids
                    .as_ref()
                    .is_none_or(|service_ids| service_ids.contains(&session.service_id))
            })
            .collect()
    }

    /// Queues a target-checked control action for one live RTMP session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session disappeared, its revision changed, its role changed, or
    /// another incompatible control request is pending.
    pub fn request_session_control(
        &self,
        session_id: SessionId,
        action: RtmpSessionControlAction,
        expected_revision: u64,
    ) -> Result<RtmpSessionControlOutcome, RtmpSessionControlError> {
        if self.service_ids.is_some()
            && !self
                .registry
                .session_snapshots()
                .into_iter()
                .any(|session| {
                    session.session_id == session_id
                        && self
                            .service_ids
                            .as_ref()
                            .is_some_and(|service_ids| service_ids.contains(&session.service_id))
                })
        {
            return Err(RtmpSessionControlError::NotFound);
        }
        self.registry
            .request_session_control(session_id, action, expected_revision)
    }

    /// Starts one exact manual recorder.
    ///
    /// # Errors
    ///
    /// Returns the catalog or recorder transition error reported by the active runtime.
    pub fn start_recording(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) -> Result<RecorderSnapshot, CatalogError> {
        if self.service_ids.is_some() {
            let Some(service) = self.service_for_stream(stream_id) else {
                return Err(CatalogError::StreamNotFound(stream_id));
            };
            if !service.runtime.admission_is_open() {
                return Err(CatalogError::AdmissionClosed);
            }
        }
        self.registry
            .start_recording(stream_id, recorder_id, at_unix_ms)
    }

    /// Stops one exact manual recorder.
    ///
    /// # Errors
    ///
    /// Returns the catalog or recorder transition error reported by the active runtime.
    pub fn stop_recording(
        &self,
        stream_id: StreamId,
        recorder_id: RecorderId,
        at_unix_ms: u64,
    ) -> Result<RecorderSnapshot, CatalogError> {
        if self.service_ids.is_some() && self.service_for_stream(stream_id).is_none() {
            return Err(CatalogError::StreamNotFound(stream_id));
        }
        self.registry
            .stop_recording(stream_id, recorder_id, at_unix_ms)
    }

    fn service_for_stream(&self, stream_id: StreamId) -> Option<&RtmpServiceHandle> {
        let snapshot = self.registry.snapshot();
        let service_id = snapshot
            .streams
            .iter()
            .find(|stream| stream.id == stream_id)
            .map(|stream| stream.key.server_id.as_str())?;
        self.services
            .iter()
            .find(|service| service.service_id() == service_id)
    }

    /// Closes new RTMP admission for the services represented by this handle while retaining
    /// existing session ownership for drain.
    pub fn close_admission(&self) {
        for service in self.services.iter() {
            service.runtime.close_service_admission();
        }
    }
}

impl From<Arc<RtmpRegistry>> for RtmpControlHandle {
    fn from(registry: Arc<RtmpRegistry>) -> Self {
        Self {
            media_catalog: Arc::new(MediaCatalog::default()),
            registry,
            services: Arc::new(Vec::new()),
            service_ids: None,
            vod_catalog: Arc::new(VodCatalog::default()),
        }
    }
}

/// Bounded completion handle for all recorder lifecycles in one RTMP runtime set.
#[derive(Clone)]
pub struct RtmpShutdown {
    lifecycles: Arc<Vec<RtmpRecorderLifecycle>>,
}

impl Default for RtmpShutdown {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl RtmpShutdown {
    fn new(lifecycles: Vec<RtmpRecorderLifecycle>) -> Self {
        Self {
            lifecycles: Arc::new(lifecycles),
        }
    }

    /// Returns true when both handles represent the same runtime-set shutdown authority.
    #[must_use]
    pub fn is_same_lifecycle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lifecycles, &other.lifecycles)
    }

    /// Closes every recorder lifecycle represented by this handle by the supplied deadline.
    pub fn initiate(&self, deadline: Instant) {
        for lifecycle in self.lifecycles.iter() {
            drop(lifecycle.initiate_shutdown(deadline));
        }
    }

    /// Returns true when every recorder lifecycle has completed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.lifecycles
            .iter()
            .all(RtmpRecorderLifecycle::is_complete)
    }

    /// Waits until every recorder lifecycle completes or the supplied absolute deadline expires.
    #[must_use]
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let mut complete = true;
        for lifecycle in self.lifecycles.iter() {
            complete &= lifecycle.shutdown_handle().wait_until(deadline);
        }
        complete && self.is_complete()
    }
}

/// Started RTMP services plus their opaque service and control handles.
#[derive(Clone)]
pub struct RtmpRuntimeSet {
    services: Vec<RtmpServiceHandle>,
    service_index: BTreeMap<String, usize>,
    control: RtmpControlHandle,
    shutdown: RtmpShutdown,
}

/// Linear owner of RTMP service plans that have completed preparation but have not been started.
pub struct PreparedRtmpRuntimeSet {
    mode: RtmpPrepareMode,
    registry: Arc<RtmpRegistry>,
    services: Vec<PreparedRtmpService>,
    #[cfg(test)]
    start_failure: Option<String>,
    #[cfg(test)]
    start_events: Arc<Mutex<Vec<String>>>,
}

struct PreparedRtmpService {
    media_applications: Vec<(String, String, Arc<MediaApplication>)>,
    service_id: String,
    preparation: RtmpServicePreparation,
    vod_applications: Vec<Arc<VodApplication>>,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RtmpRuntimeSetError {
    #[error("RTMP service `{service_id}` is registered more than once")]
    DuplicateService { service_id: String },
    #[error("RTMP service `{service_id}` uses a different registry")]
    RegistryMismatch { service_id: String },
    #[error("RTMP service `{service_id}` preparation timed out")]
    PreparationTimedOut { service_id: String },
    #[error("RTMP service `{service_id}` could not prepare {resource}")]
    Preparation {
        service_id: String,
        resource: &'static str,
    },
    #[error("validated RTMP plans cannot be started without activation preparation")]
    ValidationOnly,
    #[error("RTMP service `{service_id}` start timed out")]
    StartTimedOut { service_id: String },
    #[error("RTMP service `{service_id}` could not start")]
    Start { service_id: String },
}

impl PreparedRtmpRuntimeSet {
    /// Prepares RTMP service plans in canonical input order without starting runtime workers.
    ///
    /// Duplicate service identifiers are rejected before any plan acquisition. Media and recorder
    /// stores are shared across the complete set. Validation mode performs preflight only;
    /// activation mode retains the acquired resources needed by [`Self::start`].
    ///
    /// # Errors
    ///
    /// Returns a contextual preparation error when identifiers are duplicated, the deadline has
    /// expired, or a plan resource cannot be acquired.
    pub fn prepare(
        plans: impl IntoIterator<Item = RtmpServicePlan>,
        context: &RtmpPrepareContext,
        deadline: Instant,
    ) -> Result<Self, RtmpRuntimeSetError> {
        let plans: Vec<_> = plans.into_iter().collect();
        let capabilities = RtmpCapabilities {
            live_ingest: !plans.is_empty(),
            manual_recording: plans.iter().any(|plan| {
                plan.applications().iter().any(|application| {
                    application
                        .recorders()
                        .iter()
                        .any(|recorder| recorder.start() == crate::RtmpRecorderStart::Manual)
                })
            }),
        };
        let mut service_ids = BTreeSet::new();
        for plan in &plans {
            let service_id = plan.service_id().to_owned();
            if !service_ids.insert(service_id.clone()) {
                return Err(RtmpRuntimeSetError::DuplicateService { service_id });
            }
        }

        let mut media_stores = RtmpMediaStoreRegistry::default();
        let mut recorder_stores = RtmpRecorderStoreRegistry::default();
        let mut services = Vec::with_capacity(plans.len());
        for plan in plans {
            ensure_preparation_deadline(plan.service_id(), deadline)?;
            services.push(prepare_service(
                &plan,
                context,
                deadline,
                &mut media_stores,
                &mut recorder_stores,
            )?);
        }
        Ok(Self {
            mode: context.mode(),
            registry: Arc::new(RtmpRegistry::new(capabilities)),
            services,
            #[cfg(test)]
            start_failure: None,
            #[cfg(test)]
            start_events: Arc::default(),
        })
    }

    /// Returns the number of prepared services.
    #[must_use]
    pub fn len(&self) -> usize {
        self.services.len()
    }

    /// Returns true when no service plans were prepared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    /// Consumes activation preparation and starts every service against its retained registry.
    ///
    /// Services start in canonical plan order. A timeout or start failure closes admission and
    /// rolls already-started services back in reverse order, bounded by `deadline`.
    ///
    /// # Errors
    ///
    /// Returns an error for validation-only preparation, an expired start deadline, or a service
    /// start failure.
    pub fn start(self, deadline: Instant) -> Result<RtmpRuntimeSet, RtmpRuntimeSetError> {
        if self.mode == RtmpPrepareMode::Validation {
            return Err(RtmpRuntimeSetError::ValidationOnly);
        }
        let mut runtimes = Vec::with_capacity(self.services.len());
        let mut media_applications = Vec::new();
        let mut vod_applications = Vec::new();
        for service in self.services {
            #[cfg(test)]
            self.start_events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("start:{}", service.service_id));
            let start = if Instant::now() >= deadline {
                Err(RtmpRuntimeSetError::StartTimedOut {
                    service_id: service.service_id.clone(),
                })
            } else {
                #[cfg(test)]
                if self.start_failure.as_deref() == Some(&service.service_id) {
                    Err(RtmpRuntimeSetError::Start {
                        service_id: service.service_id.clone(),
                    })
                } else {
                    service
                        .preparation
                        .start(Arc::clone(&self.registry))
                        .map_err(|_| RtmpRuntimeSetError::Start {
                            service_id: service.service_id.clone(),
                        })
                }
                #[cfg(not(test))]
                service
                    .preparation
                    .start(Arc::clone(&self.registry))
                    .map_err(|_| RtmpRuntimeSetError::Start {
                        service_id: service.service_id.clone(),
                    })
            };
            match start {
                Ok(runtime) => {
                    media_applications.extend(service.media_applications);
                    vod_applications.extend(service.vod_applications);
                    runtimes.push(runtime);
                }
                Err(error) => {
                    while let Some(runtime) = runtimes.pop() {
                        #[cfg(test)]
                        self.start_events
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!("rollback:{}", runtime.service_id()));
                        rollback_runtime(runtime, deadline);
                    }
                    return Err(error);
                }
            }
        }
        RtmpRuntimeSet::from_started_with_catalogs(
            self.registry,
            runtimes,
            VodCatalog::from_applications(vod_applications),
            Arc::new(MediaCatalog::from_applications(media_applications)),
        )
    }

    #[cfg(test)]
    fn fail_start_for(mut self, service_id: impl Into<String>) -> Self {
        self.start_failure = Some(service_id.into());
        self
    }

    #[cfg(test)]
    fn start_events(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.start_events)
    }
}

#[allow(clippy::too_many_lines)]
fn prepare_service(
    plan: &RtmpServicePlan,
    context: &RtmpPrepareContext,
    deadline: Instant,
    media_stores: &mut RtmpMediaStoreRegistry,
    recorder_stores: &mut RtmpRecorderStoreRegistry,
) -> Result<PreparedRtmpService, RtmpRuntimeSetError> {
    let service_id = plan.service_id();
    let callbacks = prepare_callbacks(plan.callbacks(), plan.outbound_policy(), service_id)?;
    let mut applications = Vec::with_capacity(plan.applications().len());
    let mut media_applications = Vec::new();
    let mut service_hub = None;
    let mut vod_applications = Vec::new();
    for application in plan.applications() {
        ensure_preparation_deadline(service_id, deadline)?;
        let hub = application.fanout().runtime_hub();
        service_hub.get_or_insert_with(|| hub.clone());
        let relay = application.relay();
        let push_targets = relay
            .push()
            .iter()
            .map(|target| {
                ensure_preparation_deadline(service_id, deadline)?;
                relay.acquire_push_target(
                    target,
                    context.candidate_listener_addresses().iter().copied(),
                    || prepare_client_options(target.client(), service_id),
                    |_| preparation_error(service_id, "push target"),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pull_targets = relay
            .pull()
            .iter()
            .map(|target| {
                ensure_preparation_deadline(service_id, deadline)?;
                relay.acquire_pull_target(
                    target,
                    context.candidate_listener_addresses().iter().copied(),
                    || prepare_client_options(target.client(), service_id),
                    |_| preparation_error(service_id, "pull target"),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let application_callbacks =
            prepare_callbacks(application.callbacks(), plan.outbound_policy(), service_id)?;
        let vod = application
            .vod()
            .map(|vod| vod.acquire(service_id, application.name()))
            .transpose()
            .map_err(|_| preparation_error(service_id, "VOD application"))?
            .map(Arc::new);
        vod_applications.extend(vod.iter().cloned());
        let hls = if let Some(hls) = application.media().and_then(RtmpMediaPlan::hls) {
            media_stores
                .prepare(
                    hls.root_directory(),
                    hls.media_store_limits(),
                    context.mode(),
                )
                .map_err(|_| preparation_error(service_id, "HLS store"))?
                .map(|store| hls.build_output(store))
        } else {
            None
        };
        let dash = if let Some(dash) = application.media().and_then(RtmpMediaPlan::dash) {
            media_stores
                .prepare(
                    dash.root_directory(),
                    dash.media_store_limits(),
                    context.mode(),
                )
                .map_err(|_| preparation_error(service_id, "DASH store"))?
                .map(|store| dash.build_output(store))
        } else {
            None
        };
        let media = RtmpMediaPlan::combine_outputs(hls, dash);
        if let Some(media) = &media {
            media_applications.push((
                service_id.to_owned(),
                application.name().to_owned(),
                media.clone(),
            ));
        }
        let recorders = application
            .recorders()
            .iter()
            .map(|recorder| {
                let store = recorder_stores
                    .prepare(
                        recorder.root_directory(),
                        recorder.store_limits(),
                        context.mode(),
                        Some(deadline),
                    )
                    .map_err(|_| {
                        if Instant::now() >= deadline {
                            RtmpRuntimeSetError::PreparationTimedOut {
                                service_id: service_id.to_owned(),
                            }
                        } else {
                            preparation_error(service_id, "recorder store")
                        }
                    })?;
                Ok(store.map(|store| recorder.build_policy(store)))
            })
            .collect::<Result<Vec<_>, RtmpRuntimeSetError>>()?
            .into_iter()
            .flatten();
        applications.push(application.build_runtime_application(
            hub,
            push_targets,
            pull_targets,
            application_callbacks,
            vod,
            media,
            application.exec().iter().map(|plan| plan.profile().clone()),
            recorders,
        ));
    }
    let policy = RtmpSessionPolicy::with_session_limits(
        applications,
        plan.outbound_chunk_size(),
        plan.inbound_limits(),
    );
    let mut preparation = RtmpServicePreparation::new(
        service_id,
        service_hub.unwrap_or_else(|| LiveHub::new(LiveHubLimits::default())),
        policy,
    )
    .with_callbacks(callbacks);
    if let Some(auto_push) = plan.auto_push() {
        preparation = preparation
            .with_auto_push(auto_push.config().clone())
            .map_err(|_| preparation_error(service_id, "auto-push policy"))?;
    }
    Ok(PreparedRtmpService {
        media_applications,
        service_id: service_id.to_owned(),
        preparation,
        vod_applications,
    })
}

fn prepare_callbacks(
    plan: &RtmpCallbackPlan,
    outbound_policy: &crate::RtmpOutboundPolicy,
    service_id: &str,
) -> Result<RtmpCallbackPolicy, RtmpRuntimeSetError> {
    let mut endpoints = [
        RtmpCallbackEventPlan::Connect,
        RtmpCallbackEventPlan::Disconnect,
        RtmpCallbackEventPlan::Publish,
        RtmpCallbackEventPlan::PublishDone,
        RtmpCallbackEventPlan::Play,
        RtmpCallbackEventPlan::PlayDone,
        RtmpCallbackEventPlan::Done,
        RtmpCallbackEventPlan::Update,
    ]
    .map(|event| plan.acquire_endpoint(event, outbound_policy))
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .map_err(|_| preparation_error(service_id, "callback endpoint"))?
    .into_iter();
    Ok(RtmpCallbackPolicy {
        on_connect: endpoints.next().expect("callback slot"),
        on_disconnect: endpoints.next().expect("callback slot"),
        on_publish: endpoints.next().expect("callback slot"),
        on_publish_done: endpoints.next().expect("callback slot"),
        on_play: endpoints.next().expect("callback slot"),
        on_play_done: endpoints.next().expect("callback slot"),
        on_done: endpoints.next().expect("callback slot"),
        on_update: endpoints.next().expect("callback slot"),
        method: plan.method(),
        timeout: plan.timeout(),
        update_timeout: plan.update_timeout(),
        update_strict: plan.update_strict(),
        relay_redirect: plan.relay_redirect(),
    })
}

fn prepare_client_options(
    plan: &crate::RtmpClientPlan,
    service_id: &str,
) -> Result<RtmpClientOptions, RtmpRuntimeSetError> {
    let credential = plan
        .credential()
        .map(|reference| {
            let secret = fs::read(reference.secret_file())
                .map_err(|_| preparation_error(service_id, "relay credential"))?;
            if secret.is_empty()
                || secret.len() > 4 * 1_024
                || secret.iter().any(u8::is_ascii_control)
                || std::str::from_utf8(&secret).is_err()
            {
                return Err(preparation_error(service_id, "relay credential"));
            }
            Ok(RtmpCredential::new(reference.username(), secret))
        })
        .transpose()?;
    Ok(RtmpClientOptions {
        flash_version: plan.flash_version().to_owned(),
        playback_buffer_ms: plan.playback_buffer_ms(),
        tc_url: plan.tc_url().map(str::to_owned),
        credential,
    })
}

fn ensure_preparation_deadline(
    service_id: &str,
    deadline: Instant,
) -> Result<(), RtmpRuntimeSetError> {
    if Instant::now() >= deadline {
        Err(RtmpRuntimeSetError::PreparationTimedOut {
            service_id: service_id.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn preparation_error(service_id: &str, resource: &'static str) -> RtmpRuntimeSetError {
    RtmpRuntimeSetError::Preparation {
        service_id: service_id.to_owned(),
        resource,
    }
}

fn rollback_runtime(runtime: RtmpServiceRuntime, deadline: Instant) {
    runtime.close_service_admission();
    if let Some(shutdown) = runtime.initiate_recorder_shutdown_scoped(deadline) {
        let _ = shutdown.wait_until(deadline);
    }
    drop(runtime);
}

impl RtmpRuntimeSet {
    /// Builds a handle set around already-started service runtimes.
    ///
    /// This is the transitional adapter for the staged runtime composition path. Planning and
    /// resource acquisition remain in their existing owners until the full RTMP composition root
    /// migration is complete.
    ///
    /// # Errors
    ///
    /// Returns an error when service identifiers are duplicated or a service belongs to another
    /// registry.
    pub fn from_started(
        registry: Arc<RtmpRegistry>,
        runtimes: impl IntoIterator<Item = RtmpServiceRuntime>,
    ) -> Result<Self, RtmpRuntimeSetError> {
        Self::from_started_with_catalogs(
            registry,
            runtimes,
            Arc::new(VodCatalog::default()),
            Arc::new(MediaCatalog::default()),
        )
    }

    fn from_started_with_catalogs(
        registry: Arc<RtmpRegistry>,
        runtimes: impl IntoIterator<Item = RtmpServiceRuntime>,
        vod_catalog: Arc<VodCatalog>,
        media_catalog: Arc<MediaCatalog>,
    ) -> Result<Self, RtmpRuntimeSetError> {
        let mut services = Vec::new();
        let mut service_index = BTreeMap::new();
        for runtime in runtimes {
            let service_id = runtime.service_id().to_owned();
            if !Arc::ptr_eq(runtime.registry(), &registry) {
                return Err(RtmpRuntimeSetError::RegistryMismatch { service_id });
            }
            if service_index.contains_key(&service_id) {
                return Err(RtmpRuntimeSetError::DuplicateService { service_id });
            }
            service_index.insert(service_id, services.len());
            services.push(RtmpServiceHandle::new(runtime));
        }
        let control =
            RtmpControlHandle::new(registry, services.clone(), vod_catalog, media_catalog);
        let shutdown = RtmpShutdown::new(recorder_lifecycles(&services));
        Ok(Self {
            services,
            service_index,
            control,
            shutdown,
        })
    }

    /// Returns an opaque handle for one configured service.
    #[must_use]
    pub fn service(&self, service_id: &str) -> Option<RtmpServiceHandle> {
        self.service_index
            .get(service_id)
            .and_then(|index| self.services.get(*index))
            .cloned()
    }

    /// Returns the opaque control-plane handle for this runtime set.
    #[must_use]
    pub fn control(&self) -> RtmpControlHandle {
        self.control.clone()
    }

    /// Closes admission and begins bounded recorder shutdown for every service.
    #[must_use]
    pub fn begin_shutdown(&self, deadline: Instant) -> RtmpShutdown {
        self.control.close_admission();
        let shutdown = self.shutdown_handle();
        shutdown.initiate(deadline);
        shutdown
    }

    /// Returns generation-independent shutdown authority for this runtime set.
    #[must_use]
    pub fn shutdown_handle(&self) -> RtmpShutdown {
        self.shutdown.clone()
    }
}

fn recorder_lifecycles(services: &[RtmpServiceHandle]) -> Vec<RtmpRecorderLifecycle> {
    let mut lifecycles = Vec::new();
    for service in services {
        if let Some(lifecycle) = service.runtime.recorder_lifecycle()
            && !lifecycles
                .iter()
                .any(|existing: &RtmpRecorderLifecycle| existing.is_same_lifecycle(&lifecycle))
        {
            lifecycles.push(lifecycle);
        }
    }
    lifecycles
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use tempfile::tempdir;

    use super::*;
    use crate::{
        LiveHub, LiveHubLimits, RecorderWorkerConfig, RecordingPathPolicy, RecordingStore,
        RecordingStoreLimits, RtmpAccessPlan, RtmpApplication, RtmpApplicationPlan,
        RtmpCallbackPlan, RtmpCapabilities, RtmpFanoutPlan, RtmpOutboundPolicy, RtmpRecorderPolicy,
        RtmpRecorderStart, RtmpRelayPlan, RtmpServicePlan, RtmpSessionCeilings, RtmpSessionLimits,
        RtmpSessionPolicy,
    };

    fn registry() -> Arc<RtmpRegistry> {
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        }))
    }

    fn runtime(registry: Arc<RtmpRegistry>) -> RtmpServiceRuntime {
        runtime_named("edge", registry)
    }

    fn runtime_named(service_id: &str, registry: Arc<RtmpRegistry>) -> RtmpServiceRuntime {
        RtmpServiceRuntime::new(
            service_id,
            registry,
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::new([RtmpApplication::new("live", true, true)]),
        )
    }

    fn service_plan(service_id: &str) -> RtmpServicePlan {
        let relay = RtmpRelayPlan::new(
            RtmpOutboundPolicy::default(),
            8,
            4_096,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(30),
            Duration::from_secs(1),
            Duration::from_secs(1),
            [],
            [],
        )
        .expect("relay plan");
        let application = RtmpApplicationPlan::new(
            "live",
            true,
            true,
            RtmpAccessPlan::default(),
            RtmpAccessPlan::default(),
            RtmpSessionCeilings::new(16, 4, 12),
            RtmpFanoutPlan::new(12, 8, 4_096).expect("fanout plan"),
            relay,
            None,
            [],
            None,
            RtmpCallbackPlan::default(),
            [],
        )
        .expect("application plan");
        RtmpServicePlan::new(
            service_id,
            4_096,
            RtmpSessionLimits::default(),
            RtmpCallbackPlan::default(),
            [application],
            None,
        )
        .expect("service plan")
    }

    #[test]
    fn staged_preparation_is_linear_and_validation_only_cannot_start() {
        let context = RtmpPrepareContext::new(RtmpPrepareMode::Validation, []);
        let prepared = PreparedRtmpRuntimeSet::prepare(
            [service_plan("zulu"), service_plan("alpha")],
            &context,
            Instant::now() + Duration::from_secs(1),
        )
        .expect("validated runtime set");

        assert_eq!(prepared.len(), 2);
        assert!(!prepared.is_empty());
        assert!(matches!(
            prepared.start(Instant::now() + Duration::from_secs(1)),
            Err(RtmpRuntimeSetError::ValidationOnly)
        ));
    }

    #[test]
    fn activation_starts_in_canonical_order_against_one_registry() {
        let context = RtmpPrepareContext::new(RtmpPrepareMode::Activation, []);
        let prepared = PreparedRtmpRuntimeSet::prepare(
            [service_plan("zulu"), service_plan("alpha")],
            &context,
            Instant::now() + Duration::from_secs(1),
        )
        .expect("prepared runtime set");
        let set = prepared
            .start(Instant::now() + Duration::from_secs(1))
            .expect("started runtime set");

        assert_eq!(
            set.services
                .iter()
                .map(RtmpServiceHandle::service_id)
                .collect::<Vec<_>>(),
            ["zulu", "alpha"]
        );
        assert!(
            set.services
                .iter()
                .all(|service| Arc::ptr_eq(service.runtime.registry(), &set.control.registry))
        );
    }

    #[test]
    fn duplicate_plans_and_expired_preparation_are_rejected() {
        let context = RtmpPrepareContext::new(RtmpPrepareMode::Activation, []);
        let Err(duplicate) = PreparedRtmpRuntimeSet::prepare(
            [service_plan("edge"), service_plan("edge")],
            &context,
            Instant::now() + Duration::from_secs(1),
        ) else {
            panic!("duplicate service plans were accepted")
        };
        assert_eq!(
            duplicate,
            RtmpRuntimeSetError::DuplicateService {
                service_id: "edge".into()
            }
        );
        let Err(expired) =
            PreparedRtmpRuntimeSet::prepare([service_plan("edge")], &context, Instant::now())
        else {
            panic!("expired preparation was accepted")
        };
        assert_eq!(
            expired,
            RtmpRuntimeSetError::PreparationTimedOut {
                service_id: "edge".into()
            }
        );

        let prepared = PreparedRtmpRuntimeSet::prepare(
            [service_plan("edge")],
            &context,
            Instant::now() + Duration::from_secs(1),
        )
        .expect("prepared runtime set");
        let Err(expired) = prepared.start(Instant::now()) else {
            panic!("expired start was accepted")
        };
        assert_eq!(
            expired,
            RtmpRuntimeSetError::StartTimedOut {
                service_id: "edge".into()
            }
        );
    }

    #[test]
    fn partial_start_failure_rolls_back_in_reverse_order() {
        let context = RtmpPrepareContext::new(RtmpPrepareMode::Activation, []);
        let prepared = PreparedRtmpRuntimeSet::prepare(
            [
                service_plan("alpha"),
                service_plan("beta"),
                service_plan("charlie"),
            ],
            &context,
            Instant::now() + Duration::from_secs(1),
        )
        .expect("prepared runtime set")
        .fail_start_for("charlie");
        let events = prepared.start_events();

        let Err(failure) = prepared.start(Instant::now() + Duration::from_secs(1)) else {
            panic!("injected start failure was ignored")
        };
        assert_eq!(
            failure,
            RtmpRuntimeSetError::Start {
                service_id: "charlie".into()
            }
        );
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            [
                "start:alpha",
                "start:beta",
                "start:charlie",
                "rollback:beta",
                "rollback:alpha",
            ]
        );
    }

    #[test]
    fn empty_prepared_set_starts_and_shuts_down_within_its_deadline() {
        let context = RtmpPrepareContext::new(RtmpPrepareMode::Activation, []);
        let prepared =
            PreparedRtmpRuntimeSet::prepare([], &context, Instant::now() + Duration::from_secs(1))
                .expect("empty prepared set");
        assert!(prepared.is_empty());

        let deadline = Instant::now() + Duration::from_secs(1);
        let set = prepared.start(deadline).expect("empty runtime set");
        let shutdown = set.begin_shutdown(deadline);
        assert!(shutdown.is_complete());
        assert!(shutdown.wait_until(deadline));
    }

    #[test]
    fn service_lookup_and_control_share_the_started_runtime_authority() {
        let registry = registry();
        let set = RtmpRuntimeSet::from_started(Arc::clone(&registry), [runtime(registry)])
            .expect("runtime set");
        let service = set.service("edge").expect("service handle");
        assert_eq!(service.service_id(), "edge");

        let session = service.session();
        let snapshot = set
            .control()
            .session_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.session_id == session.session_id())
            .expect("session snapshot");
        assert!(!snapshot.connected);
        drop(session);
        assert!(set.control().session_snapshots().is_empty());
    }

    #[test]
    fn control_handle_forwards_target_checked_session_requests() {
        let registry = registry();
        let set = RtmpRuntimeSet::from_started(Arc::clone(&registry), [runtime(registry)])
            .expect("runtime set");
        let session = set.service("edge").expect("service").session();
        let snapshot = set
            .control()
            .session_snapshots()
            .into_iter()
            .find(|candidate| candidate.session_id == session.session_id())
            .expect("session snapshot");

        let control = set.control();
        assert!(
            !control
                .request_session_control(
                    session.session_id(),
                    RtmpSessionControlAction::Client,
                    snapshot.revision,
                )
                .expect("request control")
                .already_requested
        );
        assert!(
            control
                .request_session_control(
                    session.session_id(),
                    RtmpSessionControlAction::Client,
                    snapshot.revision,
                )
                .expect("repeat control")
                .already_requested
        );
    }

    #[test]
    fn control_closes_owned_admission_without_dropping_existing_session_ownership() {
        let registry = registry();
        let set = RtmpRuntimeSet::from_started(Arc::clone(&registry), [runtime(registry.clone())])
            .expect("runtime set");
        let owned = set.service("edge").expect("owned service");
        let session = set.service("edge").expect("service").session();

        set.control().close_admission();
        assert!(!owned.runtime.admission_is_open());
        assert!(
            registry
                .attach_subscriber(
                    crate::StreamKey::new("edge", "live", "camera"),
                    SessionId::new(),
                    1,
                )
                .is_ok()
        );
        assert!(
            set.control()
                .session_snapshots()
                .iter()
                .any(|snapshot| snapshot.session_id == session.session_id())
        );
    }

    #[test]
    fn empty_runtime_set_does_not_close_foreign_registry_admission() {
        let registry = registry();
        let set =
            RtmpRuntimeSet::from_started(Arc::clone(&registry), []).expect("empty runtime set");

        set.control().close_admission();
        assert!(
            registry
                .attach_subscriber(
                    crate::StreamKey::new("edge", "live", "camera"),
                    SessionId::new(),
                    1,
                )
                .is_ok()
        );
    }

    #[test]
    fn runtime_set_rejects_duplicate_and_foreign_service_runtime_ids() {
        let expected_registry = registry();
        let Err(duplicate) = RtmpRuntimeSet::from_started(
            Arc::clone(&expected_registry),
            [
                runtime(Arc::clone(&expected_registry)),
                runtime(Arc::clone(&expected_registry)),
            ],
        ) else {
            panic!("duplicate service was accepted")
        };
        assert_eq!(
            duplicate,
            RtmpRuntimeSetError::DuplicateService {
                service_id: "edge".into()
            }
        );

        let foreign_registry = registry();
        let Err(foreign) =
            RtmpRuntimeSet::from_started(expected_registry, [runtime(foreign_registry)])
        else {
            panic!("foreign registry was accepted")
        };
        assert_eq!(
            foreign,
            RtmpRuntimeSetError::RegistryMismatch {
                service_id: "edge".into()
            }
        );
    }

    #[test]
    fn control_handle_scopes_snapshots_and_targets_to_its_runtime_set() {
        let registry = registry();
        let runtime_a = runtime_named("alpha", Arc::clone(&registry));
        let runtime_b = runtime_named("beta", Arc::clone(&registry));
        let session_a = runtime_a.session();
        let session_b = runtime_b.session();
        let stream_a = registry
            .attach_publisher(
                crate::StreamKey::new("alpha", "live", "camera"),
                session_a.session_id(),
                Vec::new(),
                1,
            )
            .expect("alpha publisher");
        let stream_b = registry
            .attach_publisher(
                crate::StreamKey::new("beta", "live", "camera"),
                session_b.session_id(),
                Vec::new(),
                1,
            )
            .expect("beta publisher");
        let set = RtmpRuntimeSet::from_started(registry, [runtime_a]).expect("runtime set");
        let control = set.control();

        assert_eq!(
            control
                .catalog_snapshot()
                .streams
                .iter()
                .map(|stream| stream.key.server_id.as_str())
                .collect::<Vec<_>>(),
            ["alpha"]
        );
        assert_eq!(
            control
                .session_snapshots()
                .iter()
                .map(|session| session.service_id.as_str())
                .collect::<Vec<_>>(),
            ["alpha"]
        );
        assert_eq!(
            control.request_session_control(
                session_b.session_id(),
                RtmpSessionControlAction::Client,
                0,
            ),
            Err(RtmpSessionControlError::NotFound)
        );
        assert_eq!(
            control.start_recording(stream_b, RecorderId::new(), 1),
            Err(CatalogError::StreamNotFound(stream_b))
        );
        assert_eq!(
            control.stop_recording(stream_b, RecorderId::new(), 1),
            Err(CatalogError::StreamNotFound(stream_b))
        );
        assert_eq!(stream_a, control.catalog_snapshot().streams[0].id);
    }

    #[test]
    fn control_admission_close_does_not_close_foreign_registry_admission() {
        let registry = registry();
        let runtime_a = runtime_named("alpha", Arc::clone(&registry));
        let runtime_b = runtime_named("beta", Arc::clone(&registry));
        let set =
            RtmpRuntimeSet::from_started(Arc::clone(&registry), [runtime_a]).expect("runtime set");

        set.control().close_admission();
        assert!(runtime_b.admission_is_open());
        assert!(
            registry
                .attach_subscriber(
                    crate::StreamKey::new("beta", "live", "camera"),
                    SessionId::new(),
                    1,
                )
                .is_ok()
        );
        drop(runtime_b);
    }

    #[test]
    fn runtime_set_retains_canonical_input_order_for_shutdown() {
        let registry = registry();
        let set = RtmpRuntimeSet::from_started(
            registry.clone(),
            [
                runtime_named("zulu", Arc::clone(&registry)),
                runtime_named("alpha", registry),
            ],
        )
        .expect("runtime set");

        assert_eq!(
            set.services
                .iter()
                .map(RtmpServiceHandle::service_id)
                .collect::<Vec<_>>(),
            ["zulu", "alpha"]
        );
    }

    #[test]
    fn shutdown_retains_canonical_service_order() {
        let zulu_root = tempdir().expect("zulu recording root");
        let alpha_root = tempdir().expect("alpha recording root");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let recorder = |name: &str, root: &std::path::Path| {
            RtmpRecorderPolicy::new(
                name,
                RtmpRecorderStart::Continuous,
                RecordingStore::open(
                    root,
                    RecordingStoreLimits {
                        max_bytes: Some(1024),
                        max_files: Some(4),
                        max_active_recorders: 1,
                    },
                )
                .expect("recording store"),
                RecordingPathPolicy::new(".flv", false).expect("recording path policy"),
                RecorderWorkerConfig::default(),
            )
        };
        let runtime = |service_id: &str, recorder| {
            RtmpServiceRuntime::new(
                service_id,
                Arc::clone(&registry),
                LiveHub::new(LiveHubLimits::default()),
                RtmpSessionPolicy::new([RtmpApplication::with_runtime(
                    "live",
                    true,
                    false,
                    LiveHub::new(LiveHubLimits::default()),
                    [],
                    [recorder],
                )]),
            )
        };
        let set = RtmpRuntimeSet::from_started(
            Arc::clone(&registry),
            [
                runtime("zulu", recorder("zulu", zulu_root.path())),
                runtime("alpha", recorder("alpha", alpha_root.path())),
            ],
        )
        .expect("runtime set");
        let zulu = set
            .service("zulu")
            .expect("zulu service")
            .runtime
            .recorder_lifecycle()
            .expect("zulu recorder lifecycle");
        let alpha = set
            .service("alpha")
            .expect("alpha service")
            .runtime
            .recorder_lifecycle()
            .expect("alpha recorder lifecycle");
        let deadline = Instant::now() + Duration::from_secs(1);
        let shutdown = set.begin_shutdown(deadline);
        drop(zulu.initiate_shutdown(deadline));
        drop(alpha.initiate_shutdown(deadline));

        assert_eq!(shutdown.lifecycles.len(), 2);
        assert!(shutdown.lifecycles[0].is_same_lifecycle(&zulu));
        assert!(shutdown.lifecycles[1].is_same_lifecycle(&alpha));
        assert!(shutdown.wait_until(deadline));
    }

    #[test]
    fn shutdown_is_bounded_and_idempotent_for_started_recorders() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024),
                max_files: Some(4),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let recorder = RtmpRecorderPolicy::new(
            "archive",
            RtmpRecorderStart::Continuous,
            store,
            RecordingPathPolicy::new(".flv", false).expect("recording path policy"),
            RecorderWorkerConfig::default(),
        );
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let runtime = RtmpServiceRuntime::new(
            "edge",
            Arc::clone(&registry),
            LiveHub::new(LiveHubLimits::default()),
            RtmpSessionPolicy::new([RtmpApplication::with_runtime(
                "live",
                true,
                false,
                LiveHub::new(LiveHubLimits::default()),
                [],
                [recorder],
            )]),
        );
        let set = RtmpRuntimeSet::from_started(registry, [runtime]).expect("runtime set");
        let deadline = Instant::now() + Duration::from_secs(1);
        let first = set.begin_shutdown(deadline);
        let second = set.begin_shutdown(deadline);
        assert!(first.wait_until(deadline));
        assert!(second.is_complete());
    }
}
