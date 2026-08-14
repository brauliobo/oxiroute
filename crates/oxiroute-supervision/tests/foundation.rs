use std::{
    cell::Cell,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use oxiroute_supervision::{
    BoundError, BoundedString, BoundedVec, CatalogError, CorrelationError, CorrelationTable, Epoch,
    EventEnvelope, GenerationId, GenerationLaunchDocument, GenerationRole, IdentityError,
    InstanceId, Lifecycle, LifecycleControl, LifecycleOperation, LifecycleRequest, MetricBatch,
    MetricBatchError, MetricDescriptor, MetricId, MetricKind, MetricRegistry, MetricRegistryError,
    MetricSample, MetricValue, ProtocolError, RequestEnvelope, RequestId, ResponseEnvelope,
    Revision, RpcOutcome, Sequence, ServiceId, ServiceProtocol, SnapshotEnvelope,
    SupervisedGenerationCatalog,
};
use serde::{
    Deserialize,
    de::value::{Error as ValueError, SeqDeserializer},
};

#[test]
fn bounded_values_enforce_limits_and_deserialization() {
    let value = BoundedString::<4>::new("rust").unwrap();
    assert_eq!(value.as_str(), "rust");
    assert_eq!(
        BoundedString::<4>::new("route").unwrap_err(),
        BoundError::StringTooLong {
            actual: 5,
            maximum: 4,
        }
    );
    assert_eq!(
        BoundedVec::<_, 2>::from_slice(&[1, 2, 3]).unwrap_err(),
        BoundError::VectorTooLong {
            actual: 3,
            maximum: 2,
        }
    );
    assert!(serde_json::from_str::<BoundedString<3>>(r#""four""#).is_err());
    assert!(serde_json::from_str::<BoundedVec<u8, 1>>("[1,2]").is_err());
}

#[test]
fn bounded_sequence_rejects_size_hint_before_reading_elements() {
    let reads = Rc::new(Cell::new(0));
    let observed_reads = Rc::clone(&reads);
    let values = [1_u8, 2].into_iter().inspect(move |_| {
        observed_reads.set(observed_reads.get() + 1);
    });
    let deserializer = SeqDeserializer::<_, ValueError>::new(values);

    assert!(BoundedVec::<u8, 1>::deserialize(deserializer).is_err());
    assert_eq!(reads.get(), 0);
}

static DESERIALIZED_ELEMENTS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct Counted;

impl<'de> Deserialize<'de> for Counted {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        u8::deserialize(deserializer)?;
        DESERIALIZED_ELEMENTS.fetch_add(1, Ordering::SeqCst);
        Ok(Self)
    }
}

#[test]
fn bounded_sequence_aborts_incrementally_at_overflow() {
    DESERIALIZED_ELEMENTS.store(0, Ordering::SeqCst);
    assert!(serde_json::from_str::<BoundedVec<Counted, 2>>("[1,2,3,4]").is_err());
    assert_eq!(DESERIALIZED_ELEMENTS.load(Ordering::SeqCst), 2);
}

#[test]
fn identities_are_typed_bounded_and_wire_transparent() {
    let service = ServiceId::new("edge.http-1").unwrap();
    let instance = InstanceId::new("worker_01").unwrap();
    assert_eq!(serde_json::to_string(&service).unwrap(), r#""edge.http-1""#);
    assert_eq!(serde_json::to_string(&instance).unwrap(), r#""worker_01""#);
    assert_eq!(ServiceId::new(""), Err(IdentityError::Empty));
    assert!(matches!(
        InstanceId::new("worker/01"),
        Err(IdentityError::InvalidCharacter { .. })
    ));
}

#[test]
fn lifecycle_accepts_explicit_snapshot_and_reactivation_phases() {
    let retirement = [
        Lifecycle::Quiescing,
        Lifecycle::Draining,
        Lifecycle::Snapshotting,
        Lifecycle::Stopping,
        Lifecycle::Stopped,
    ];
    for states in retirement.windows(2) {
        assert_eq!(states[0].transition(states[1]).unwrap(), states[1]);
    }
    assert_eq!(
        Lifecycle::Quiescing
            .transition(Lifecycle::Reactivating)
            .unwrap()
            .transition(Lifecycle::Active)
            .unwrap(),
        Lifecycle::Active
    );
    assert!(Lifecycle::Draining.transition(Lifecycle::Stopping).is_err());
    assert!(Lifecycle::Quiescing.transition(Lifecycle::Active).is_err());
}

fn launch_document(
    instance: &str,
    generation: u64,
    revision: u64,
    payload: &str,
) -> GenerationLaunchDocument<Revision, String> {
    GenerationLaunchDocument::new(
        InstanceId::new(instance).unwrap(),
        GenerationId(generation),
        Revision(revision),
        payload.to_owned(),
    )
}

#[test]
fn supervised_catalog_preserves_roles_and_monotonic_generation_identity() {
    let mut catalog =
        SupervisedGenerationCatalog::new(launch_document("active-1", 1, 10, "active"));
    assert_eq!(catalog.allocate_generation().unwrap(), GenerationId(2));

    catalog
        .begin_candidate(launch_document("candidate-2", 2, 20, "candidate"))
        .unwrap();
    assert_eq!(
        catalog.get(GenerationRole::Candidate).unwrap().payload(),
        "candidate"
    );
    assert_eq!(
        catalog.begin_candidate(launch_document("candidate-3", 3, 30, "second")),
        Err(CatalogError::CandidateInProgress)
    );

    assert_eq!(catalog.commit_candidate().unwrap(), None);
    assert_eq!(catalog.active().instance_id().as_str(), "candidate-2");
    assert_eq!(
        catalog.previous().unwrap().instance_id().as_str(),
        "active-1"
    );
    assert_eq!(catalog.allocate_generation().unwrap(), GenerationId(3));
    assert_eq!(
        catalog
            .documents()
            .map(|(role, document)| (role, document.generation_id()))
            .collect::<Vec<_>>(),
        vec![
            (GenerationRole::Active, GenerationId(2)),
            (GenerationRole::Previous, GenerationId(1)),
        ]
    );
}

#[test]
fn supervised_catalog_consumes_multiple_reservations_once_in_monotonic_order() {
    let mut catalog =
        SupervisedGenerationCatalog::new(launch_document("active-1", 1, 10, "active"));
    assert_eq!(catalog.allocate_generation(), Ok(GenerationId(2)));
    assert_eq!(catalog.allocate_generation(), Ok(GenerationId(3)));

    catalog
        .begin_candidate(launch_document("candidate-2", 2, 20, "first"))
        .unwrap();
    catalog.quarantine_candidate().unwrap();
    catalog
        .begin_candidate(launch_document("candidate-3", 3, 30, "second"))
        .unwrap();
    catalog.quarantine_candidate().unwrap();

    assert_eq!(
        catalog.begin_candidate(launch_document("candidate-2-reuse", 2, 40, "reuse")),
        Err(CatalogError::StaleGeneration {
            current: GenerationId(3),
            candidate: GenerationId(2),
        })
    );
}

#[test]
fn supervised_catalog_quarantines_candidates_and_rejects_reused_or_stale_documents() {
    let mut catalog =
        SupervisedGenerationCatalog::new(launch_document("active-1", 1, 10, "active"));
    catalog
        .begin_candidate(launch_document("candidate-2", 2, 20, "bad"))
        .unwrap();
    assert_eq!(catalog.quarantine_candidate().unwrap(), None);
    assert_eq!(
        catalog.quarantined().unwrap().instance_id().as_str(),
        "candidate-2"
    );
    assert_eq!(
        catalog.begin_candidate(launch_document("candidate-3", 3, 30, "next")),
        Ok(())
    );
    assert_eq!(
        catalog
            .quarantine_candidate()
            .unwrap()
            .unwrap()
            .instance_id()
            .as_str(),
        "candidate-2"
    );
    assert_eq!(
        catalog.record_restart_required(launch_document("restart-3", 3, 30, "restart")),
        Err(CatalogError::StaleGeneration {
            current: GenerationId(3),
            candidate: GenerationId(3),
        })
    );
    assert_eq!(
        catalog.begin_candidate(launch_document("restart-3", 3, 30, "stale")),
        Err(CatalogError::StaleGeneration {
            current: GenerationId(3),
            candidate: GenerationId(3),
        })
    );
    assert_eq!(
        catalog.record_restart_required(launch_document("restart-4", 4, 40, "restart")),
        Ok(None)
    );
    assert_eq!(
        catalog.record_restart_required(launch_document("active-1", 5, 50, "duplicate")),
        Err(CatalogError::DuplicateInstanceId {
            instance_id: InstanceId::new("active-1").unwrap(),
        })
    );
}

#[test]
fn supervised_catalog_rejects_generation_reuse_after_releasing_retained_documents() {
    let mut previous_catalog =
        SupervisedGenerationCatalog::new(launch_document("active-1", 1, 10, "active"));
    previous_catalog
        .begin_candidate(launch_document("candidate-2", 2, 20, "candidate"))
        .unwrap();
    assert_eq!(previous_catalog.commit_candidate().unwrap(), None);
    assert_eq!(
        previous_catalog.take_previous().unwrap().generation_id(),
        GenerationId(1)
    );
    assert!(matches!(
        previous_catalog.begin_candidate(launch_document("previous-reuse", 1, 30, "reuse")),
        Err(CatalogError::StaleGeneration {
            current: GenerationId(2),
            candidate: GenerationId(1),
        })
    ));

    let mut quarantined_catalog =
        SupervisedGenerationCatalog::new(launch_document("active-1", 1, 10, "active"));
    quarantined_catalog
        .begin_candidate(launch_document("candidate-3", 3, 30, "candidate"))
        .unwrap();
    quarantined_catalog.quarantine_candidate().unwrap();
    assert_eq!(
        quarantined_catalog
            .take_quarantined()
            .unwrap()
            .generation_id(),
        GenerationId(3)
    );
    assert!(matches!(
        quarantined_catalog.begin_candidate(launch_document("quarantined-reuse", 3, 40, "reuse")),
        Err(CatalogError::StaleGeneration {
            current: GenerationId(3),
            candidate: GenerationId(3),
        })
    ));

    let mut restart_required_catalog =
        SupervisedGenerationCatalog::new(launch_document("active-1", 1, 10, "active"));
    restart_required_catalog
        .record_restart_required(launch_document("restart-4", 4, 40, "restart"))
        .unwrap();
    assert_eq!(
        restart_required_catalog
            .take_restart_required()
            .unwrap()
            .generation_id(),
        GenerationId(4)
    );
    assert!(matches!(
        restart_required_catalog.record_restart_required(launch_document(
            "restart-reuse",
            4,
            50,
            "reuse"
        )),
        Err(CatalogError::StaleGeneration {
            current: GenerationId(4),
            candidate: GenerationId(4),
        })
    ));
}

#[test]
fn supervised_catalog_allocator_exhausts_without_reusing_u64_max() {
    let maximum = u64::MAX;
    let mut catalog = SupervisedGenerationCatalog::new(launch_document(
        "active-max-minus-one",
        maximum - 1,
        10,
        "active",
    ));

    assert_eq!(catalog.allocate_generation(), Ok(GenerationId(maximum)));
    assert_eq!(
        catalog.allocate_generation(),
        Err(CatalogError::GenerationExhausted)
    );
    catalog
        .begin_candidate(launch_document("active-max", maximum, 20, "candidate"))
        .unwrap();
    catalog.quarantine_candidate().unwrap();
    assert!(matches!(
        catalog.begin_candidate(launch_document("maximum-reuse", maximum, 30, "reuse")),
        Err(CatalogError::StaleGeneration {
            current: GenerationId(value),
            candidate: GenerationId(candidate),
        }) if value == maximum && candidate == maximum
    ));
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestLifecycleControl;

impl LifecycleControl for TestLifecycleControl {
    type Revision = Revision;
    type Status = &'static str;
    type Outcome = LifecycleOperation;
    type Error = std::convert::Infallible;

    fn status(&self) -> Self::Status {
        "active"
    }

    fn execute(
        &self,
        request: LifecycleRequest<Self::Revision>,
        _timeout: Option<Duration>,
    ) -> Result<Self::Outcome, Self::Error> {
        Ok(request.operation())
    }
}

#[test]
fn lifecycle_control_builds_mode_neutral_revision_precondition_requests() {
    let control = TestLifecycleControl;
    assert_eq!(control.status(), "active");
    assert_eq!(
        control.request_reload(&Revision(7)),
        LifecycleRequest::new(LifecycleOperation::Reload, Revision(7))
    );
    assert_eq!(
        control.request_rollback(&Revision(7)).operation(),
        LifecycleOperation::Rollback
    );
    assert_eq!(
        control.request_drain(&Revision(7)).expected_revision(),
        &Revision(7)
    );
    assert_eq!(
        serde_json::to_string(&control.request_shutdown(&Revision(7))).unwrap(),
        r#"{"operation":"shutdown","expected_revision":7}"#
    );
    assert_eq!(
        control.execute(control.request_reload(&Revision(7)), None),
        Ok(LifecycleOperation::Reload)
    );
}

#[derive(Debug, Eq, PartialEq)]
struct TestProtocol;

impl ServiceProtocol for TestProtocol {
    const VERSION: u16 = 2;
    type Request = String;
    type Response = String;
    type Error = String;
    type Event = String;
    type Snapshot = Vec<u8>;
}

struct ZeroVersionProtocol;

impl ServiceProtocol for ZeroVersionProtocol {
    const VERSION: u16 = 0;
    type Request = String;
    type Response = String;
    type Error = String;
    type Event = String;
    type Snapshot = Vec<u8>;
}

fn token() -> oxiroute_supervision::CorrelationToken {
    CorrelationTable::<(), 1>::new()
        .insert(RequestId(9), Epoch(10), ())
        .unwrap()
}

#[test]
fn typed_protocol_envelopes_validate_versions_and_correlation_tokens() {
    let correlation = token();
    let request = RequestEnvelope::<TestProtocol>::new(
        correlation,
        ServiceId::new("http").unwrap(),
        InstanceId::new("http-2").unwrap(),
        GenerationId(2),
        String::from("prepare"),
    )
    .unwrap();
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(
        serde_json::from_str::<RequestEnvelope<TestProtocol>>(&json).unwrap(),
        request
    );

    let response = ResponseEnvelope::<TestProtocol>::new(
        correlation,
        ServiceId::new("http").unwrap(),
        InstanceId::new("http-2").unwrap(),
        GenerationId(2),
        RpcOutcome::Success(String::from("ready")),
    )
    .unwrap();
    assert_eq!(response.correlation(), correlation);
    let json = serde_json::to_string(&response).unwrap();
    assert_eq!(
        serde_json::from_str::<ResponseEnvelope<TestProtocol>>(&json).unwrap(),
        response
    );

    let event = EventEnvelope::<TestProtocol>::new(
        Sequence(3),
        ServiceId::new("http").unwrap(),
        InstanceId::new("http-2").unwrap(),
        GenerationId(2),
        String::from("ready"),
    )
    .unwrap();
    assert_eq!(event.sequence(), Sequence(3));

    let malformed = json.replace("\"protocol_version\":2", "\"protocol_version\":3");
    assert!(serde_json::from_str::<RequestEnvelope<TestProtocol>>(&malformed).is_err());
    assert!(matches!(
        RequestEnvelope::<ZeroVersionProtocol>::new(
            correlation,
            ServiceId::new("http").unwrap(),
            InstanceId::new("http-2").unwrap(),
            GenerationId(2),
            String::from("prepare"),
        ),
        Err(ProtocolError::ZeroVersion)
    ));
}

#[test]
fn correlation_reuse_cannot_accept_a_late_response() {
    let mut table = CorrelationTable::<&str, 2>::new();
    let old = table.insert(RequestId(1), Epoch(10), "old").unwrap();
    assert_eq!(table.complete(old).unwrap().value, "old");
    let current = table.insert(RequestId(1), Epoch(20), "current").unwrap();

    assert_ne!(old.generation(), current.generation());
    assert_eq!(
        table.complete(old),
        Err(CorrelationError::Unknown { token: old })
    );
    assert_eq!(table.complete(current).unwrap().value, "current");
}

#[test]
fn correlation_table_rejects_pending_duplicates_and_capacity() {
    let mut table = CorrelationTable::<&str, 2>::new();
    table.insert(RequestId(1), Epoch(10), "one").unwrap();
    assert_eq!(
        table.insert(RequestId(1), Epoch(20), "duplicate"),
        Err(CorrelationError::Duplicate {
            request_id: RequestId(1),
        })
    );
    table.insert(RequestId(2), Epoch(30), "two").unwrap();
    assert_eq!(
        table.insert(RequestId(3), Epoch(40), "three"),
        Err(CorrelationError::Full { maximum: 2 })
    );
}

#[test]
fn correlation_timeouts_return_full_tokens_in_request_order() {
    let mut table = CorrelationTable::<&str, 3>::new();
    let three = table.insert(RequestId(3), Epoch(9), "three").unwrap();
    let one = table.insert(RequestId(1), Epoch(10), "one").unwrap();
    table.insert(RequestId(2), Epoch(11), "two").unwrap();

    let expired = table.expire(Epoch(10));
    assert_eq!(
        expired
            .iter()
            .map(|(token, pending)| (*token, pending.value))
            .collect::<Vec<_>>(),
        vec![(one, "one"), (three, "three")]
    );
}

#[test]
fn snapshots_validate_deserialization_and_expose_read_only_fields() {
    assert!(serde_json::from_str::<SnapshotEnvelope<Vec<u8>>>(
        r#"{"format_version":0,"service_id":"http","generation_id":3,"revision":7,"payload":[1]}"#
    )
    .is_err());
    let snapshot = SnapshotEnvelope::new(
        1,
        ServiceId::new("http").unwrap(),
        GenerationId(3),
        Revision(7),
        vec![1_u8, 2, 3],
    )
    .unwrap();
    assert_eq!(snapshot.format_version(), 1);
    assert_eq!(snapshot.service_id().as_str(), "http");
    assert_eq!(snapshot.generation_id(), GenerationId(3));
    assert_eq!(snapshot.revision(), Revision(7));
    assert_eq!(snapshot.payload(), &[1, 2, 3]);
    let json = serde_json::to_string(&snapshot).unwrap();
    assert_eq!(
        serde_json::from_str::<SnapshotEnvelope<Vec<u8>>>(&json).unwrap(),
        snapshot
    );
}

static METRICS: &[MetricDescriptor] = &[
    MetricDescriptor {
        id: MetricId(1),
        name: "requests_total",
        unit: "requests",
        description: "Accepted requests",
        kind: MetricKind::Counter,
    },
    MetricDescriptor {
        id: MetricId(2),
        name: "connections",
        unit: "connections",
        description: "Current connections",
        kind: MetricKind::Gauge,
    },
];

fn sample(metric_id: u32, value: MetricValue) -> MetricSample {
    MetricSample {
        metric_id: MetricId(metric_id),
        value,
    }
}

#[test]
fn metric_batches_validate_registry_ids_duplicates_and_kinds() {
    let registry = MetricRegistry::new(METRICS).unwrap();
    let samples = [
        sample(1, MetricValue::Counter(12)),
        sample(2, MetricValue::Gauge(3.0)),
    ];
    let batch = MetricBatch::<2>::from_slice(registry, Sequence(4), &samples).unwrap();
    assert_eq!(batch.sequence(), Sequence(4));
    assert_eq!(batch.samples(), samples);
    assert_eq!(
        MetricBatch::<2>::from_slice(
            registry,
            Sequence(4),
            &[sample(99, MetricValue::Counter(1))],
        ),
        Err(MetricBatchError::UnknownMetric { id: MetricId(99) })
    );
    assert_eq!(
        MetricBatch::<2>::from_slice(
            registry,
            Sequence(4),
            &[
                sample(1, MetricValue::Counter(1)),
                sample(1, MetricValue::Counter(2)),
            ],
        ),
        Err(MetricBatchError::DuplicateMetric { id: MetricId(1) })
    );
    assert_eq!(
        MetricBatch::<2>::from_slice(registry, Sequence(4), &[sample(1, MetricValue::Gauge(1.0))],),
        Err(MetricBatchError::KindMismatch {
            id: MetricId(1),
            expected: MetricKind::Counter,
            actual: MetricKind::Gauge,
        })
    );
}

#[test]
fn metric_batch_deserialization_is_registry_validated() {
    let registry = MetricRegistry::new(METRICS).unwrap();
    let valid =
        r#"{"sequence":4,"samples":[{"metric_id":1,"value":{"kind":"counter","value":12}}]}"#;
    let mut deserializer = serde_json::Deserializer::from_str(valid);
    let batch = registry
        .deserialize_batch::<_, 2>(&mut deserializer)
        .unwrap();
    assert_eq!(batch.samples().len(), 1);

    let invalid =
        r#"{"sequence":4,"samples":[{"metric_id":2,"value":{"kind":"counter","value":12}}]}"#;
    let mut deserializer = serde_json::Deserializer::from_str(invalid);
    assert!(
        registry
            .deserialize_batch::<_, 2>(&mut deserializer)
            .is_err()
    );
}

#[test]
fn metric_registry_rejects_duplicate_numeric_ids() {
    static DUPLICATES: &[MetricDescriptor] = &[
        MetricDescriptor {
            id: MetricId(1),
            name: "first",
            unit: "items",
            description: "First",
            kind: MetricKind::Counter,
        },
        MetricDescriptor {
            id: MetricId(1),
            name: "second",
            unit: "items",
            description: "Second",
            kind: MetricKind::Counter,
        },
    ];
    assert_eq!(
        MetricRegistry::new(DUPLICATES).unwrap_err(),
        MetricRegistryError::DuplicateId { id: MetricId(1) }
    );
}
