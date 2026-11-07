use super::*;
use oxiroute_supervision::{GenerationLaunchDocument, Revision};

fn status() -> WorkerStatus {
    WorkerStatus {
        sequence: 1,
        generation_id: GenerationId(7),
        lifecycle: WorkerLifecycle::Active,
        administrative_state: WorkerAdministrativeState::Ready,
        accepting: true,
        runtime_started: true,
        runtime_failed: false,
        drained: false,
        generation: WorkerGenerationStatus {
            disk_revision: Some("a".repeat(64)),
            candidate_revision: None,
            active_revision: Some("b".repeat(64)),
            previous_revision: None,
            quarantined_revision: None,
            active_accepting: true,
            degraded: false,
            last_failure: None,
            prepares: 1,
            activations: 1,
            failures: 0,
            rollbacks: 0,
        },
        metrics: Some(WorkerMetrics {
            accepted_connections: 2,
            rejected_connections: 3,
            active_connections: 4,
            bytes_received: 5,
            bytes_sent: 6,
        }),
        listeners: vec![WorkerListenerStatus {
            name: "http".into(),
            protocol: "http".into(),
            bind: "127.0.0.1:8080".into(),
            administrative_state: WorkerAdministrativeState::Ready,
            state: WorkerListenerState::Listening,
            accepted_connections: 2,
            rejected_connections: 3,
            active_connections: 4,
            bytes_received: 5,
            bytes_sent: 6,
        }],
        degraded: false,
        degradation: None,
        event_cursor: 1,
        event_cursor_lost: false,
        events: vec![WorkerEventRecord {
            cursor: 1,
            timestamp_unix_ms: Some(9),
            event: "generation_activate".into(),
            outcome: "applied".into(),
            revision: Some("b".repeat(64)),
            certificate: None,
            correlation_id: Some("op-1".into()),
            source: Some("runtime".into()),
            operation: Some("generation_activate".into()),
        }],
    }
}

#[test]
fn status_encoding_preserves_bounded_observation() {
    let expected = status();
    let payload = encode_status(&expected).expect("encode");
    assert!(!payload.is_empty());
    assert_eq!(expected.sequence, 1);
}

#[test]
fn status_encoding_rejects_unbounded_event_batches() {
    let mut status = status();
    status.events = (1..=MAX_STATUS_EVENTS + 1)
        .map(|cursor| WorkerEventRecord {
            cursor: u64::try_from(cursor).expect("cursor"),
            timestamp_unix_ms: None,
            event: "event".into(),
            outcome: "applied".into(),
            revision: None,
            certificate: None,
            correlation_id: None,
            source: None,
            operation: None,
        })
        .collect();
    assert!(matches!(
        encode_status(&status),
        Err(StatusProtocolError::CollectionTooLong { kind: "events" })
    ));
}

#[test]
fn status_prefix_and_tags_stay_byte_exact() {
    let payload = encode_status(&status()).expect("encode status");
    assert_eq!(
        &payload[..23],
        &[
            0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 3, 3, 0
        ]
    );
}

fn document(
    instance: &str,
    generation: u64,
    revision: u64,
) -> GenerationLaunchDocument<Revision, ()> {
    GenerationLaunchDocument::new(
        InstanceId::new(instance).unwrap(),
        GenerationId(generation),
        Revision(revision),
        (),
    )
}

fn replacement_catalog() -> SupervisedGenerationCatalog<Revision, ()> {
    let mut catalog = SupervisedGenerationCatalog::new(document("retired", 1, 10));
    catalog.begin_candidate(document("active", 2, 20)).unwrap();
    catalog.commit_candidate().unwrap();
    catalog
}

#[test]
fn snapshot_separates_catalog_and_process_observations_and_sums_live_values() {
    let catalog = replacement_catalog();
    let mut active = status();
    active.generation_id = GenerationId(2);
    active.metrics = Some(WorkerMetrics {
        accepted_connections: 10,
        rejected_connections: 2,
        active_connections: 4,
        bytes_received: 100,
        bytes_sent: 200,
    });
    active.listeners[0].accepted_connections = 10;
    active.listeners[0].active_connections = 4;
    let mut retired = status();
    retired.generation_id = GenerationId(1);
    retired.metrics = Some(WorkerMetrics {
        accepted_connections: 7,
        rejected_connections: 1,
        active_connections: 2,
        bytes_received: 70,
        bytes_sent: 90,
    });
    retired.listeners[0].accepted_connections = 7;
    retired.listeners[0].active_connections = 2;
    retired.degraded = true;
    retired.degradation = Some("drain delayed".into());
    let active_id = InstanceId::new("active").unwrap();
    let retired_id = InstanceId::new("retired").unwrap();
    let mut history = SupervisorSnapshotHistory::new();
    history.record_events(
        GenerationRole::Active,
        &active_id,
        GenerationId(2),
        0,
        &active,
    );
    history.record_events(
        GenerationRole::Previous,
        &retired_id,
        GenerationId(1),
        0,
        &retired,
    );
    let sources = [
        WorkerSnapshotSource {
            role: GenerationRole::Active,
            instance_id: &active_id,
            generation_id: GenerationId(2),
            status: &active,
        },
        WorkerSnapshotSource {
            role: GenerationRole::Previous,
            instance_id: &retired_id,
            generation_id: GenerationId(1),
            status: &retired,
        },
    ];

    let snapshot = history.snapshot(&catalog, &sources).unwrap();

    assert_eq!(
        snapshot
            .generations
            .iter()
            .map(|generation| (generation.role, generation.revision))
            .collect::<Vec<_>>(),
        vec![
            (GenerationRole::Active, Revision(20)),
            (GenerationRole::Previous, Revision(10)),
        ]
    );
    assert_eq!(snapshot.processes.len(), 2);
    assert_eq!(snapshot.listener_observations.len(), 2);
    assert_eq!(snapshot.metrics.accepted_connections, 17);
    assert_eq!(snapshot.metrics.active_connections, 6);
    assert_eq!(snapshot.listeners[0].accepted_connections, 17);
    assert_eq!(snapshot.listeners[0].active_connections, 6);
    assert!(snapshot.degraded);
    assert_eq!(snapshot.degradation[0].role, GenerationRole::Previous);
    assert_eq!(snapshot.events.len(), 2);
    assert_eq!(snapshot.events[0].role, GenerationRole::Active);
    assert_eq!(snapshot.events[1].role, GenerationRole::Previous);
}

#[test]
fn reaped_workers_keep_cumulative_baselines_but_not_active_gauges() {
    let catalog = replacement_catalog();
    let mut active = status();
    active.generation_id = GenerationId(2);
    active.metrics = Some(WorkerMetrics {
        accepted_connections: 10,
        active_connections: 4,
        ..WorkerMetrics::default()
    });
    active.listeners[0].accepted_connections = 10;
    active.listeners[0].active_connections = 4;
    let mut retired = status();
    retired.generation_id = GenerationId(1);
    retired.metrics = Some(WorkerMetrics {
        accepted_connections: 7,
        active_connections: 2,
        ..WorkerMetrics::default()
    });
    retired.listeners[0].accepted_connections = 7;
    retired.listeners[0].active_connections = 2;
    let active_id = InstanceId::new("active").unwrap();
    let mut history = SupervisorSnapshotHistory::new();
    history.reap(Some(&retired));
    let sources = [WorkerSnapshotSource {
        role: GenerationRole::Active,
        instance_id: &active_id,
        generation_id: GenerationId(2),
        status: &active,
    }];

    let snapshot = history.snapshot(&catalog, &sources).unwrap();

    assert_eq!(snapshot.metrics.accepted_connections, 17);
    assert_eq!(snapshot.metrics.active_connections, 4);
    assert_eq!(snapshot.listeners[0].accepted_connections, 17);
    assert_eq!(snapshot.listeners[0].active_connections, 4);
    history.reap(Some(&active));
    let snapshot = history.snapshot(&catalog, &[]).unwrap();
    assert_eq!(snapshot.metrics.accepted_connections, 17);
    assert_eq!(snapshot.metrics.active_connections, 0);
    assert_eq!(snapshot.listeners[0].active_connections, 0);
}

#[test]
fn snapshot_rejects_process_role_or_identity_mismatches() {
    let catalog = replacement_catalog();
    let active = status();
    let active_id = InstanceId::new("active").unwrap();
    let sources = [WorkerSnapshotSource {
        role: GenerationRole::Candidate,
        instance_id: &active_id,
        generation_id: GenerationId(2),
        status: &active,
    }];

    assert_eq!(
        SupervisorSnapshotHistory::new()
            .snapshot(&catalog, &sources)
            .unwrap_err(),
        SupervisorSnapshotError {
            instance_id: active_id,
            observed_role: GenerationRole::Candidate,
            catalog_role: Some(GenerationRole::Active),
        }
    );
}

#[test]
fn status_counters_remain_monotonic_while_gauges_use_the_latest_value() {
    let previous = status();
    let mut current = status();
    current.metrics = Some(WorkerMetrics {
        accepted_connections: 1,
        rejected_connections: 1,
        active_connections: 1,
        bytes_received: 1,
        bytes_sent: 1,
    });
    current.listeners[0].accepted_connections = 1;
    current.listeners[0].active_connections = 1;
    current.generation.activations = 0;

    retain_monotonic_status(&previous, &mut current);

    let metrics = current.metrics.unwrap();
    assert_eq!(metrics.accepted_connections, 2);
    assert_eq!(metrics.active_connections, 1);
    assert_eq!(current.listeners[0].accepted_connections, 2);
    assert_eq!(current.listeners[0].active_connections, 1);
    assert_eq!(current.generation.activations, 1);
}

#[test]
fn aggregated_events_are_bounded_with_stable_master_cursors() {
    let mut history = SupervisorSnapshotHistory::new();
    let instance_id = InstanceId::new("active").unwrap();
    for cursor in 1..=u64::try_from(MAX_AGGREGATED_EVENTS + 3).unwrap() {
        let mut current = status();
        current.event_cursor = cursor;
        current.events[0].cursor = cursor;
        history.record_events(
            GenerationRole::Active,
            &instance_id,
            GenerationId(7),
            cursor - 1,
            &current,
        );
    }

    let events = history.worker_events(0, usize::MAX);
    assert_eq!(events.len(), MAX_AGGREGATED_EVENTS);
    assert_eq!(events.first().unwrap().cursor, 4);
    assert_eq!(
        events.last().unwrap().cursor,
        u64::try_from(MAX_AGGREGATED_EVENTS + 3).unwrap()
    );
    assert!(
        events
            .windows(2)
            .all(|events| events[0].cursor < events[1].cursor)
    );

    history.next_event_cursor = u64::MAX;
    let mut final_status = status();
    final_status.event_cursor = 2;
    final_status.events[0].cursor = 2;
    history.record_events(
        GenerationRole::Active,
        &instance_id,
        GenerationId(7),
        1,
        &final_status,
    );
    final_status.event_cursor = 3;
    final_status.events[0].cursor = 3;
    history.record_events(
        GenerationRole::Active,
        &instance_id,
        GenerationId(7),
        2,
        &final_status,
    );
    let events = history.worker_events(0, usize::MAX);
    assert_eq!(events.last().unwrap().cursor, u64::MAX);
    assert!(
        events
            .windows(2)
            .all(|events| events[0].cursor < events[1].cursor)
    );
}
