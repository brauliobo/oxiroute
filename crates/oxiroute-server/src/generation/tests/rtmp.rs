#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn validation_acquires_and_releases_rtmp_resources_without_starting_runtime_effects() {
    use crate::service_plan::{reset_rtmp_stage_counts, rtmp_stage_counts};

    let parent = TempDir::new().expect("stage parent");
    let media = parent.path().join("media-do-not-create");
    let address = unused_tcp_address();
    let document = document_for(
        &serde_json::from_value(serde_json::json!({
            "version": 1,
            "listeners": [{
                "name": "ingest",
                "bind": {"type": "socket", "address": address},
                "protocol": "rtmp",
                "service": "live",
            }],
            "rtmp_services": [{
                "name": "live",
                "applications": [{
                    "name": "live",
                    "live": true,
                    "hls": {"root_directory": media},
                }],
            }],
        }))
        .unwrap(),
    );
    let manager = GenerationManager::new();
    reset_rtmp_stage_counts();

    manager
        .validate_candidate(document_for(document.validated_config.as_draft()))
        .expect("RTMP preflight and listener probes");
    assert_eq!(rtmp_stage_counts(), (1, 0));
    assert!(!media.exists(), "validation must not create the media root");

    let candidate = manager.prepare(document).expect("activation preparation");
    assert_eq!(rtmp_stage_counts(), (2, 0));
    assert!(media.is_dir(), "activation preparation opens media roots");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("runtime start reservation");
    let generation = startup.claim_runtime_start().expect("RTMP start");
    assert_eq!(rtmp_stage_counts(), (2, 1));
    assert!(generation.rtmp_service("live").is_some());
    assert!(!generation.rtmp_auto_push_status().started);
    assert!(matches!(
        startup.claim_runtime_start(),
        Err(GenerationError::CandidateSuperseded | GenerationError::RuntimePrepare)
    ));
    assert_eq!(rtmp_stage_counts(), (2, 1));
}

#[test]
fn candidate_failure_shutdown_releases_storage_and_allows_retry() {
    let root = TempDir::new().expect("recording root");
    let manager = GenerationManager::new();
    let candidate = manager
        .prepare(rtmp_failure_document(root.path(), false))
        .expect("candidate preparation");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("runtime start reservation");
    let generation = startup.claim_runtime_start().expect("runtime start");
    let later = Instant::now() + Duration::from_secs(1);
    let first = generation.initiate_rtmp_shutdown(later);
    let second = generation.initiate_rtmp_shutdown(later + Duration::from_secs(1));

    assert!(first.is_same_lifecycle(&second));
    assert!(first.wait_until(later));
    drop(startup);
    manager.quarantine(&candidate, "runtime_start");
    drop(generation);
    drop(candidate);

    let retry = GenerationManager::new()
        .prepare(rtmp_failure_document(root.path(), false))
        .expect("recording ownership retry");
    drop(retry);
}

#[test]
fn published_generation_uses_normal_shutdown() {
    let root = TempDir::new().expect("recording root");
    let manager = GenerationManager::new();
    let candidate = manager
        .prepare(rtmp_failure_document(root.path(), false))
        .expect("candidate preparation");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("runtime start reservation");
    let generation = startup.claim_runtime_start().expect("runtime start");
    assert!(generation.mark_runtime_started());
    startup.activate().expect("candidate publication");

    assert!(manager.shutdown(Duration::from_secs(1)));
}

#[test]
fn retirement_registry_deduplicates_and_prunes_completed_lifecycles() {
    let root = TempDir::new().expect("recording root");
    let manager = GenerationManager::new();
    let candidate = manager
        .prepare(rtmp_failure_document(root.path(), false))
        .expect("candidate preparation");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("runtime start reservation");
    let generation = startup.claim_runtime_start().expect("runtime start");
    let retirement = generation.rtmp_retirement();
    let mut registry = RtmpRetirementRegistry::default();

    registry.retire(retirement.clone());
    registry.retire(retirement);
    assert_eq!(registry.len(), 1);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut work = registry.take_shutdown_work();
    assert_eq!(work.len(), 1);
    let shutdown = work.pop().expect("retirement work").initiate(deadline);
    let mut repeated_work = registry.take_shutdown_work();
    assert_eq!(repeated_work.len(), 1);
    let repeated_shutdown = repeated_work
        .pop()
        .expect("repeated retirement work")
        .initiate(deadline);
    assert!(shutdown.is_same_lifecycle(&repeated_shutdown));
    assert!(shutdown.wait_until(deadline));
    registry.prune_completed();
    assert_eq!(registry.len(), 0);
}

#[test]
fn manager_shutdown_retires_evicted_lifecycles_without_process_handles() {
    let root = TempDir::new().expect("recording root");
    let manager = GenerationManager::new();
    let first = manager
        .prepare(rtmp_failure_document(root.path(), false))
        .expect("first candidate");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(rtmp_failure_document(root.path(), false))
        .expect("second candidate");
    manager.activate(&second).expect("second activation");

    let deadline = Instant::now() + Duration::from_secs(30);
    drop(manager.begin_shutdown(deadline));
    assert!(manager.shutdown(Duration::from_secs(30)));
}

#[test]
fn rtmp_services_start_in_plan_order_and_partial_failure_rolls_back_in_reverse() {
    use crate::service_plan::{
        reset_rtmp_stage_counts, rtmp_start_events, with_rtmp_start_failure,
    };

    let first = unused_tcp_address();
    let second = unused_tcp_address();
    let config: ConfigDraft = serde_json::from_value(serde_json::json!({
        "version": 1,
        "listeners": [
            {
                "name": "first-ingest",
                "bind": {"type": "socket", "address": first},
                "protocol": "rtmp",
                "service": "first"
            },
            {
                "name": "second-ingest",
                "bind": {"type": "socket", "address": second},
                "protocol": "rtmp",
                "service": "second"
            }
        ],
        "rtmp_services": [
            {"name": "first", "applications": [{"name": "live", "live": true}]},
            {"name": "second", "applications": [{"name": "live", "live": true}]}
        ]
    }))
    .expect("ordered RTMP config");
    let manager = GenerationManager::new();
    reset_rtmp_stage_counts();
    let candidate = manager
        .prepare(document_for(&config))
        .expect("RTMP preparation");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("runtime start reservation");

    let result = with_rtmp_start_failure("second", || startup.claim_runtime_start());

    assert!(result.is_err());
    assert_eq!(
        rtmp_start_events(),
        ["start:first", "start:second", "rollback:first"]
    );
    assert_eq!(manager.process.listener_count(), 0);
}

#[test]
fn validation_reports_rtmp_store_and_runtime_construction_failures_without_starting() {
    use crate::service_plan::{
        RtmpRuntimeFault, reset_rtmp_stage_counts, rtmp_stage_counts, with_rtmp_runtime_fault,
    };

    for fault in [RtmpRuntimeFault::RecorderStore, RtmpRuntimeFault::AutoPush] {
        let root = TempDir::new().expect("RTMP validation root");
        let document = rtmp_failure_document(root.path(), fault == RtmpRuntimeFault::AutoPush);
        let manager = GenerationManager::new();
        reset_rtmp_stage_counts();

        let result = with_rtmp_runtime_fault(fault, || manager.validate_candidate(document));

        assert!(result.is_err(), "{fault:?} validation must fail");
        assert_eq!(rtmp_stage_counts(), (1, 0), "{fault:?}");
        assert_eq!(manager.process.listener_count(), 0);
    }
}

#[test]
fn later_rtmp_failures_roll_back_process_listener_entries_and_allow_retry() {
    use crate::service_plan::{RtmpRuntimeFault, with_rtmp_runtime_fault};

    for fault in [RtmpRuntimeFault::RecorderStore, RtmpRuntimeFault::AutoPush] {
        let root = TempDir::new().expect("recording root");
        let process = ProcessRuntime::new(None);
        let document = rtmp_failure_document(root.path(), fault == RtmpRuntimeFault::AutoPush);
        let failed = with_rtmp_runtime_fault(fault, || match fault {
            RtmpRuntimeFault::RecorderStore => PreparedGeneration::prepare_from(
                document,
                ListenerSource::BindOrReuse {
                    previous: None,
                    process: process.clone(),
                },
            )
            .map(drop),
            RtmpRuntimeFault::AutoPush => {
                let manager = GenerationManager::new();
                let candidate = manager.prepare(document)?;
                let mut startup = manager.begin_candidate_start(&candidate)?;
                let result = startup.claim_runtime_start().map(drop);
                assert_eq!(manager.process.listener_count(), 0);
                result
            }
            RtmpRuntimeFault::None => unreachable!(),
        });

        assert!(failed.is_err());
        assert_eq!(process.listener_count(), 0);

        let retry = PreparedGeneration::prepare_from(
            rtmp_failure_document(root.path(), fault == RtmpRuntimeFault::AutoPush),
            ListenerSource::BindOrReuse {
                previous: None,
                process: process.clone(),
            },
        )
        .expect("successful retry");
        let PreparationOutput::Prepared(retry) = retry else {
            panic!("runtime preparation returned validation");
        };
        assert_eq!(process.listener_count(), 1);
        drop(retry);
        assert_eq!(process.listener_count(), 0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn adopted_rtmp_failures_roll_back_metrics_descriptors_and_recording_ownership() {
    use crate::service_plan::{RtmpRuntimeFault, with_rtmp_runtime_fault};

    for fault in [RtmpRuntimeFault::RecorderStore, RtmpRuntimeFault::AutoPush] {
        let manager = GenerationManager::new_supervised();
        let active_candidate = manager.prepare(document()).expect("active candidate");
        let active = manager
            .activate(&active_candidate)
            .expect("active generation");
        let active_revision = active.revision().candidate.clone();
        let root = TempDir::new().expect("recording root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted RTMP listener");
        let address = listener.local_addr().expect("adopted RTMP address");
        let (document, descriptors) = adopted_rtmp_fixture(
            root.path(),
            address,
            listener,
            fault == RtmpRuntimeFault::AutoPush,
        );

        let failed = with_rtmp_runtime_fault(fault, || match fault {
            RtmpRuntimeFault::RecorderStore => {
                manager.prepare_adopted(document, descriptors).map(drop)
            }
            RtmpRuntimeFault::AutoPush => {
                let candidate = manager.prepare_adopted(document, descriptors)?;
                assert_eq!(manager.process.listener_count(), 1);
                assert!(std::net::TcpListener::bind(address).is_err());
                let mut startup = manager.begin_candidate_start(&candidate)?;
                let result = startup.claim_runtime_start().map(drop);
                assert!(matches!(
                    startup.claim_runtime_start(),
                    Err(GenerationError::CandidateSuperseded | GenerationError::RuntimePrepare)
                ));
                drop(startup);
                drop(candidate);
                result
            }
            RtmpRuntimeFault::None => unreachable!(),
        });

        assert!(failed.is_err());
        assert_eq!(manager.process.listener_count(), 0);
        assert_eq!(
            manager.active().unwrap().revision().candidate,
            active_revision
        );

        let retry_listener = std::net::TcpListener::bind(address)
            .expect("failed adoption released its listener descriptor");
        let (retry_document, retry_descriptors) = adopted_rtmp_fixture(
            root.path(),
            address,
            retry_listener,
            fault == RtmpRuntimeFault::AutoPush,
        );
        let retry = manager
            .prepare_adopted(retry_document, retry_descriptors)
            .expect("adopted RTMP retry preparation");
        assert_eq!(manager.process.listener_count(), 1);
        let mut startup = manager
            .begin_candidate_start(&retry)
            .expect("adopted RTMP retry reservation");
        assert!(startup.claim_runtime_start().is_ok());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn adopted_planning_failure_releases_the_consumed_descriptor_once() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted listener");
    let address = listener.local_addr().expect("adopted listener address");
    let (document, descriptors) = adopted_planning_failure_fixture(address, listener);
    PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());

    let result = PreparedGeneration::prepare_from(
        document,
        ListenerSource::Adopt {
            descriptors,
            process: ProcessRuntime::new(None),
        },
    );
    let Err(error) = result else {
        panic!("runtime planning must fail after descriptor adoption");
    };

    assert!(matches!(error, GenerationError::Plan(_)));
    assert_eq!(
        PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
        ["reservations"]
    );
    let rebound = std::net::TcpListener::bind(address)
        .expect("planning failure released the adopted descriptor");
    drop(rebound);
}

#[test]
fn generation_checks_and_listener_reservation_precede_recording_store_acquisition() {
    use crate::service_plan::{reset_rtmp_stage_counts, rtmp_stage_counts};

    let manager = GenerationManager::new();
    let active_candidate = manager.prepare(document()).expect("active candidate");
    let active = manager
        .activate(&active_candidate)
        .expect("active generation");
    let active_revision = active.revision().candidate.clone();
    let root = TempDir::new().expect("recording root");

    let mut token_failure = rtmp_failure_document(root.path(), false)
        .validated_config
        .to_draft();
    let stats_address = unused_tcp_address();
    token_failure.stats = Some(Stats {
        binds: vec![stats_address],
        admin_token_file: Some(root.path().join("missing-token")),
        pages: Vec::new(),
    });
    reset_rtmp_stage_counts();
    assert!(manager.prepare(document_for(&token_failure)).is_err());
    assert_eq!(rtmp_stage_counts(), (0, 0));
    assert_eq!(
        manager.active().unwrap().revision().candidate,
        active_revision
    );

    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
    let mut listener_failure = rtmp_failure_document(root.path(), false)
        .validated_config
        .to_draft();
    listener_failure.listeners[0].bind = ListenerBind::Socket {
        address: occupied.local_addr().expect("occupied address"),
        ipv6_only: false,
    };
    reset_rtmp_stage_counts();
    assert!(manager.prepare(document_for(&listener_failure)).is_err());
    assert_eq!(rtmp_stage_counts(), (0, 0));
    assert_eq!(
        manager.active().unwrap().revision().candidate,
        active_revision
    );
}
