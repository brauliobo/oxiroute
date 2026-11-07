#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn provisional_generation_transaction_rolls_back_in_reverse_order() {
    let config = Arc::new(document().validated_config);
    let inventory = ListenerInventory::compile(&config);
    let plan = runtime_plan(&config).expect("runtime plan");
    let acquired = acquire_runtime_services(&plan).expect("runtime acquisition");
    let reservations = ListenerReservations::prepare(&config, None).expect("reservations");
    let metrics = RuntimeMetrics::new();
    let registration = metrics
        .register_inventory(inventory.entries())
        .expect("listener registration");
    let rtmp = PreparedGeneration::prepare_rtmp(acquired.services()).expect("RTMP preparation");
    PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());
    let transaction = PreparedGenerationTransaction {
        drop_probes: vec![
            PreparedTransactionDropProbe::new("plan"),
            PreparedTransactionDropProbe::new("acquisition"),
            PreparedTransactionDropProbe::new("reservations"),
            PreparedTransactionDropProbe::new("listener_registration"),
            PreparedTransactionDropProbe::new("rtmp_prepared"),
        ],
        reservations_precede_plan: false,
        #[cfg(target_os = "linux")]
        descriptors: None,
        rtmp: Some(rtmp),
        listener_registration: Some(registration),
        reservations: Some(reservations),
        acquired: Some(acquired),
        plan: Some(plan),
    };

    drop(transaction);

    assert_eq!(
        PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
        [
            "rtmp_prepared",
            "listener_registration",
            "reservations",
            "acquisition",
            "plan",
        ]
    );
}

#[test]
fn every_prepared_transaction_boundary_rolls_back_completed_resources() {
    for (fault, expected) in [
        (
            PreparedTransactionFault::Reservations,
            vec!["reservations", "acquisition", "plan"],
        ),
        (
            PreparedTransactionFault::ListenerRegistration,
            vec![
                "listener_registration",
                "reservations",
                "acquisition",
                "plan",
            ],
        ),
        (
            PreparedTransactionFault::RtmpPreparation,
            vec![
                "rtmp_prepared",
                "listener_registration",
                "reservations",
                "acquisition",
                "plan",
            ],
        ),
    ] {
        PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());
        let process = ProcessRuntime::new(None);
        let result = with_prepared_transaction_fault(fault, || {
            PreparedGeneration::prepare_from(
                document(),
                ListenerSource::BindOrReuse {
                    previous: None,
                    process: process.clone(),
                },
            )
        });

        assert!(result.is_err(), "{fault:?}");
        assert_eq!(process.listener_count(), 0, "{fault:?}");
        assert_eq!(
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
            expected,
            "{fault:?}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn listener_sources_share_preparation_tail_without_validation_runtime_ownership() {
    use oxiroute_supervision_unix::{DescriptorManifest, DescriptorSet};

    let process = ProcessRuntime::new(None);
    let normal = PreparedGeneration::prepare_from(
        document(),
        ListenerSource::BindOrReuse {
            previous: None,
            process: process.clone(),
        },
    )
    .expect("normal preparation");
    assert!(matches!(normal, PreparationOutput::Prepared(_)));
    drop(normal);
    assert_eq!(process.listener_count(), 0);

    let validation =
        PreparedGeneration::prepare_from(document(), ListenerSource::Validate { previous: None })
            .expect("validation preparation");
    assert!(matches!(validation, PreparationOutput::Validated));

    let manifest = DescriptorManifest::new(Vec::new()).expect("empty descriptor manifest");
    let descriptors = DescriptorSet::new(&manifest, Vec::new()).expect("empty descriptor set");
    let adopted = PreparedGeneration::prepare_from(
        document(),
        ListenerSource::Adopt {
            descriptors,
            process: process.clone(),
        },
    )
    .expect("adopted preparation");
    assert!(matches!(adopted, PreparationOutput::Prepared(_)));
    drop(adopted);
    assert_eq!(process.listener_count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn adopted_common_tail_faults_release_metrics_plan_and_descriptors_in_reverse_order() {
    for (fault, expected) in [
        (
            PreparedTransactionFault::Reservations,
            vec!["acquisition", "plan", "reservations"],
        ),
        (
            PreparedTransactionFault::ListenerRegistration,
            vec![
                "listener_registration",
                "acquisition",
                "plan",
                "reservations",
            ],
        ),
        (
            PreparedTransactionFault::RtmpPreparation,
            vec![
                "rtmp_prepared",
                "listener_registration",
                "acquisition",
                "plan",
                "reservations",
            ],
        ),
    ] {
        let root = TempDir::new().expect("adopted fault root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted fault listener");
        let address = listener.local_addr().expect("adopted fault address");
        let (document, descriptors) = adopted_rtmp_fixture(root.path(), address, listener, false);
        let process = ProcessRuntime::new(None);
        PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());

        let result = with_prepared_transaction_fault(fault, || {
            PreparedGeneration::prepare_from(
                document,
                ListenerSource::Adopt {
                    descriptors,
                    process: process.clone(),
                },
            )
        });

        assert!(result.is_err(), "{fault:?}");
        assert_eq!(process.listener_count(), 0, "{fault:?}");
        assert_eq!(
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
            expected,
            "{fault:?}"
        );
        std::net::TcpListener::bind(address).expect("adopted fault released its descriptor");
    }
}

#[test]
fn adapter_generation_handle_retains_root_and_tears_it_down_exactly_once() {
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let PreparationOutput::Prepared(mut prepared) = PreparedGeneration::prepare_from(
        document(),
        ListenerSource::BindOrReuse {
            previous: None,
            process: ProcessRuntime::new(None),
        },
    )
    .expect("prepared generation") else {
        panic!("runtime preparation returned validation");
    };
    prepared.resources.set_drop_probe(Arc::clone(&drops));
    let generation = Arc::new(RuntimeGeneration::activate(prepared));
    let adapter = Arc::clone(&generation);

    drop(generation);
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    drop(adapter);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn plan_error_preserves_code_and_redacted_rtmp_source_chain() {
    use std::error::Error as _;

    let source = oxiroute_rtmp::RtmpServicePlan::new(
        "streaming",
        4_096,
        oxiroute_rtmp::RtmpSessionLimits::default().with_max_inbound_message_size(0),
        oxiroute_rtmp::RtmpCallbackPlan::default(),
        [],
        None,
    )
    .unwrap_err();
    let error = GenerationError::Plan(Box::new(crate::ServicePlanError::RtmpPreparation(
        Box::new(source),
    )));

    assert_eq!(error.code(), "service_plan_prepare");
    assert!(error.source().is_some());
    assert!(error.source().unwrap().source().is_some());
    let rendered = error.to_string();
    assert!(rendered.contains("invalid RTMP Bound at service.inbound_limits"));
    assert!(rendered.contains("service `streaming`"));
    for secret in [
        "/secret/tenant/key.pem",
        "token=super-secret",
        "https://user:password@example.test/private",
    ] {
        assert!(!rendered.contains(secret));
    }
}
