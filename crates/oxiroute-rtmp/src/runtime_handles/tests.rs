use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tempfile::tempdir;

use super::*;
use crate::{
    LiveHub, LiveHubLimits, RecorderWorkerConfig, RecordingPathPolicy, RecordingStore,
    RecordingStoreLimits, RtmpAccessPlan, RtmpApplication, RtmpApplicationPlan, RtmpCallbackPlan,
    RtmpCapabilities, RtmpFanoutPlan, RtmpOutboundPolicy, RtmpRecorderPolicy, RtmpRecorderStart,
    RtmpRelayPlan, RtmpServicePlan, RtmpSessionCeilings, RtmpSessionLimits, RtmpSessionPolicy,
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

fn runtime_set(
    registry: Arc<RtmpRegistry>,
    runtimes: impl IntoIterator<Item = RtmpServiceRuntime>,
) -> Result<RtmpRuntimeSet, RtmpRuntimeSetError> {
    RtmpRuntimeSet::assemble(
        registry,
        runtimes,
        Arc::new(VodCatalog::default()),
        Arc::new(MediaCatalog::default()),
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

    let deadline = Instant::now() + Duration::from_secs(30);
    let set = prepared.start(deadline).expect("empty runtime set");
    let shutdown = set.begin_shutdown(deadline);
    assert!(shutdown.is_complete());
    assert!(shutdown.wait_until(deadline));
}

#[test]
fn service_lookup_and_control_share_the_started_runtime_authority() {
    let registry = registry();
    let set = runtime_set(Arc::clone(&registry), [runtime(registry)]).expect("runtime set");
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
    let set = runtime_set(Arc::clone(&registry), [runtime(registry)]).expect("runtime set");
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
    let set = runtime_set(Arc::clone(&registry), [runtime(registry.clone())]).expect("runtime set");
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
    let set = runtime_set(Arc::clone(&registry), []).expect("empty runtime set");

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
    let Err(duplicate) = runtime_set(
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
    let Err(foreign) = runtime_set(expected_registry, [runtime(foreign_registry)]) else {
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
    let set = runtime_set(registry, [runtime_a]).expect("runtime set");
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
    let set = runtime_set(Arc::clone(&registry), [runtime_a]).expect("runtime set");

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
    let set = runtime_set(
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
    let set = runtime_set(
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
    let deadline = Instant::now() + Duration::from_secs(30);
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
    let set = runtime_set(registry, [runtime]).expect("runtime set");
    let deadline = Instant::now() + Duration::from_secs(30);
    let first = set.begin_shutdown(deadline);
    let second = set.begin_shutdown(deadline);
    assert!(first.wait_until(deadline));
    assert!(second.is_complete());
}
