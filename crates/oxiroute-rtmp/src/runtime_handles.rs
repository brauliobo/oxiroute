use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    sync::Arc,
    time::Instant,
};

use crate::{
    CatalogError, RecorderId, RecorderSnapshot, RtmpAutoPushStatus, RtmpCatalogSnapshot,
    RtmpClientSnapshot, RtmpRecorderShutdown, RtmpRegistry, RtmpServiceRuntime, RtmpSession,
    RtmpSessionControlAction, RtmpSessionControlError, RtmpSessionControlOutcome, SessionId,
    StreamId,
};

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
    registry: Arc<RtmpRegistry>,
    services: Arc<Vec<RtmpServiceHandle>>,
    service_ids: Arc<BTreeSet<String>>,
}

impl RtmpControlHandle {
    fn new(registry: Arc<RtmpRegistry>, services: Vec<RtmpServiceHandle>) -> Self {
        let service_ids = services
            .iter()
            .map(|service| service.service_id().to_owned())
            .collect();
        Self {
            registry,
            services: Arc::new(services),
            service_ids: Arc::new(service_ids),
        }
    }

    /// Returns the current immutable stream catalog snapshot.
    #[must_use]
    pub fn catalog_snapshot(&self) -> Arc<RtmpCatalogSnapshot> {
        let registry_snapshot = self.registry.snapshot();
        let mut snapshot = (*registry_snapshot).clone();
        snapshot
            .streams
            .retain(|stream| self.service_ids.contains(&stream.key.server_id));
        Arc::new(snapshot)
    }

    /// Returns bounded snapshots of currently registered client sessions.
    #[must_use]
    pub fn session_snapshots(&self) -> Vec<RtmpClientSnapshot> {
        self.registry
            .session_snapshots()
            .into_iter()
            .filter(|session| self.service_ids.contains(&session.service_id))
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
        if !self
            .registry
            .session_snapshots()
            .into_iter()
            .any(|session| {
                session.session_id == session_id && self.service_ids.contains(&session.service_id)
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
        let Some(service) = self.service_for_stream(stream_id) else {
            return Err(CatalogError::StreamNotFound(stream_id));
        };
        if !service.runtime.admission_is_open() {
            return Err(CatalogError::AdmissionClosed);
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
        if self.service_for_stream(stream_id).is_none() {
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

/// Bounded completion handle for all recorder lifecycles in one RTMP runtime set.
#[derive(Clone)]
pub struct RtmpShutdown {
    recorders: Arc<Vec<RtmpRecorderShutdown>>,
}

impl RtmpShutdown {
    fn new(recorders: Vec<RtmpRecorderShutdown>) -> Self {
        Self {
            recorders: Arc::new(recorders),
        }
    }

    /// Returns true when every recorder lifecycle has completed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.recorders.iter().all(RtmpRecorderShutdown::is_complete)
    }

    /// Waits until every recorder lifecycle completes or the supplied absolute deadline expires.
    #[must_use]
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let mut complete = true;
        for recorder in self.recorders.iter() {
            complete &= recorder.wait_until(deadline);
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
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RtmpRuntimeSetError {
    #[error("RTMP service `{service_id}` is registered more than once")]
    DuplicateService { service_id: String },
    #[error("RTMP service `{service_id}` uses a different registry")]
    RegistryMismatch { service_id: String },
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
        let control = RtmpControlHandle::new(registry, services.clone());
        Ok(Self {
            services,
            service_index,
            control,
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
        let mut recorders = Vec::new();
        for service in &self.services {
            if let Some(shutdown) = service.runtime.initiate_recorder_shutdown_scoped(deadline)
                && !recorders
                    .iter()
                    .any(|existing: &RtmpRecorderShutdown| existing.is_same_lifecycle(&shutdown))
            {
                recorders.push(shutdown);
            }
        }
        RtmpShutdown::new(recorders)
    }
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
        RecordingStoreLimits, RtmpApplication, RtmpCapabilities, RtmpRecorderPolicy,
        RtmpRecorderStart, RtmpSessionPolicy,
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
        let zulu_shutdown = zulu.initiate_shutdown(deadline);
        let alpha_shutdown = alpha.initiate_shutdown(deadline);

        assert_eq!(shutdown.recorders.len(), 2);
        assert!(shutdown.recorders[0].is_same_lifecycle(&zulu_shutdown));
        assert!(shutdown.recorders[1].is_same_lifecycle(&alpha_shutdown));
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
