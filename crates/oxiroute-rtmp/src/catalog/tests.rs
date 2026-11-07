use std::{
    fs::OpenOptions,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use rustix::fs::{FlockOperation, flock};
use tempfile::tempdir;

use super::*;
use crate::{
    RecorderFailure, RecorderWorkerConfig, RecordingPathPolicy, RecordingStore,
    RecordingStoreLimits, RtmpRecorderPolicy, RtmpRecorderStart,
};

#[test]
fn publisher_activity_is_monotonic_across_metadata_and_wall_clock_rollback() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let key = StreamKey::new("edge", "live", "camera");
    let subscriber = SessionId::new();
    let stream_id = registry
        .attach_subscriber(key.clone(), subscriber, 1)
        .expect("subscriber");
    let publisher = SessionId::new();
    registry
        .attach_publisher(key.clone(), publisher, Vec::new(), 10_000)
        .expect("publisher");
    registry
        .update_media_sample(stream_id, publisher, 1, MediaSnapshot::default(), 20_000)
        .expect("metadata activity sample");
    registry
        .update_media_sample(stream_id, publisher, 2, MediaSnapshot::default(), 15_000)
        .expect("wall-clock rollback sample");

    assert!(
        registry
            .stale_publisher_owner(
                &key,
                20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS - 1,
                crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS,
            )
            .is_none()
    );
    let owner = registry
        .stale_publisher_owner(
            &key,
            20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS,
            crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS,
        )
        .expect("stale publisher");
    let shutdown = registry
        .detach_expected_publisher(owner, 20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS)
        .expect("stale publisher detach");
    shutdown.shutdown(20_000 + crate::RTMP_STALE_PUBLISHER_THRESHOLD_MS);

    let snapshot = registry.snapshot();
    let stream = snapshot
        .streams
        .iter()
        .find(|stream| stream.id == stream_id)
        .expect("subscriber keeps stream");
    assert!(stream.publisher.is_none());
    assert_eq!(stream.media, MediaSnapshot::default());
    assert!(stream.recorders.is_empty());
}

#[test]
fn terminal_admission_rejects_new_recorder_start() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let session_id = SessionId::new();
    let stream_id = registry
        .attach_publisher_inner(
            StreamKey::new("live", "broadcast", "camera"),
            session_id,
            vec![(
                RecorderDefinition::manual(Some("archive".into())),
                None::<Arc<RecorderController>>,
            )],
            Vec::new(),
            1,
        )
        .expect("publisher");
    let recorder_id = registry
        .recorder_ids(stream_id, session_id)
        .expect("recorder IDs")[0];

    registry.close_admission();

    assert!(matches!(
        registry.start_recording(stream_id, recorder_id, 2),
        Err(CatalogError::AdmissionClosed)
    ));
}

#[test]
fn reaper_backpressures_generations_while_registry_completion_is_blocked() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(8),
            max_active_recorders: 4,
        },
    )
    .expect("recording store");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
    let completing = test_controller(&store, &reaper, "completing", 1024, Duration::from_secs(1));
    let first_pending = test_controller(
        &store,
        &reaper,
        "first-pending",
        1024,
        Duration::from_secs(1),
    );
    let second_pending = test_controller(
        &store,
        &reaper,
        "second-pending",
        1024,
        Duration::from_secs(1),
    );
    let completing_context = recorder_context();
    completing
        .start(completing_context)
        .expect("start completing generation");
    first_pending
        .start(recorder_context())
        .expect("start first pending generation");
    second_pending
        .start(recorder_context())
        .expect("start second pending generation");

    let registry_lock = registry.lock();
    assert!(completing.stop(completing_context));
    wait_until(Duration::from_secs(2), || !completing.status().stopping);

    let pending = [Arc::clone(&first_pending), Arc::clone(&second_pending)];
    let (completed_tx, completed_rx) = mpsc::channel();
    let submitter = thread::spawn(move || {
        for controller in pending {
            controller.deactivate(2_000);
        }
        completed_tx.send(()).expect("report submitted generations");
    });
    assert!(
        matches!(
            completed_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "submission must backpressure while the prior generation awaits registry completion"
    );

    drop(registry_lock);
    completed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("pending generations submitted after completion");
    submitter.join().expect("generation submitter");
    wait_until(Duration::from_secs(2), || {
        !first_pending.status().stopping && !second_pending.status().stopping
    });
    drop(reaper_owner);
}

#[test]
fn restart_releases_controller_lock_while_stale_worker_awaits_reaper_capacity() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(8),
            max_active_recorders: 4,
        },
    )
    .expect("recording store");
    let ownership = OpenOptions::new()
        .read(true)
        .open(root.path())
        .expect("recording root ownership");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
    let controller = test_controller(&store, &reaper, "camera", 8, Duration::from_secs(5));

    let first_context = recorder_context();
    controller
        .start(first_context)
        .expect("start first generation");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall storage admission");
    assert_eq!(
        controller.try_enqueue(
            crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
            1_100,
        ),
        RecorderEnqueueResult::Queued
    );
    thread::sleep(Duration::from_millis(20));
    assert!(controller.stop(first_context));

    controller
        .start(recorder_context())
        .expect("start second generation");
    assert_eq!(
        controller.try_enqueue(
            crate::MediaEvent::audio(0, Arc::<[u8]>::from(vec![0; 32]))
                .expect("oversized audio event"),
            1_200,
        ),
        RecorderEnqueueResult::DroppedDiscontinuity
    );
    wait_until(Duration::from_secs(2), || {
        controller
            .status()
            .status
            .is_some_and(|status| matches!(status.phase, RecorderWorkerPhase::Failed(_)))
    });

    let restart = Arc::clone(&controller);
    let restart_context = recorder_context();
    let (started_tx, started_rx) = mpsc::channel();
    let restart_thread = thread::spawn(move || {
        started_tx
            .send(restart.start(restart_context))
            .expect("report restart result");
    });
    assert!(
        matches!(
            started_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "restart must backpressure behind the first generation"
    );

    let observed = Arc::clone(&controller);
    let (observed_tx, observed_rx) = mpsc::channel();
    let observer_thread = thread::spawn(move || {
        observed.status();
        let _ = observed_tx.send(());
    });
    observed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("restart must not retain the controller lock while backpressured");
    observer_thread.join().expect("controller observer");

    flock(&ownership, FlockOperation::Unlock).expect("release storage admission");
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("restart completes after reaper capacity is released")
        .expect("restart third generation");
    restart_thread.join().expect("restart thread");
    controller.deactivate(1_300);
    wait_until(Duration::from_secs(2), || !controller.status().stopping);
    drop(reaper_owner);
}

#[test]
fn proactive_shutdown_transitions_recording_through_stopping_to_idle() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(8),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let ownership = OpenOptions::new()
        .read(true)
        .open(root.path())
        .expect("recording root ownership");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
    let controller = test_controller(&store, &reaper, "camera", 1024, Duration::from_secs(1));
    let session_id = SessionId::new();
    let registration = registry
        .register_managed_publisher(
            StreamKey::new("edge", "live", "camera"),
            session_id,
            vec![(
                RecorderDefinition::automatic(Some("archive".to_owned())),
                Arc::clone(&controller),
            )],
            Vec::new(),
            1_000,
        )
        .expect("managed publisher");
    let stream_id = registration.stream_id();
    let recorder_id = registration.recorder_ids()[0];
    registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);
    assert!(matches!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("recording snapshot")
            .phase,
        RecorderPhase::Recording { .. }
    ));
    flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
    assert_eq!(
        controller.try_enqueue(
            crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
            1_100,
        ),
        RecorderEnqueueResult::Queued
    );
    thread::sleep(Duration::from_millis(20));

    let shutdown = reaper_owner.initiate_shutdown(Instant::now() + Duration::from_secs(1));
    registry.initiate_recorder_shutdown(&reaper);

    assert!(matches!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("stopping snapshot")
            .phase,
        RecorderPhase::Stopping { .. }
    ));
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
    assert!(shutdown.wait_until(Instant::now() + Duration::from_secs(1)));
    wait_until(Duration::from_secs(1), || {
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .is_ok_and(|snapshot| snapshot.phase == RecorderPhase::Idle)
    });
    drop(registration);
}

#[test]
fn continuous_recorder_returns_from_failed_to_recording_in_one_publisher() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(8),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
    let controller = test_controller_with_start(
        &store,
        &reaper,
        "camera",
        1024,
        Duration::from_secs(1),
        RtmpRecorderStart::Continuous,
    );
    let session_id = SessionId::new();
    let registration = registry
        .register_managed_publisher(
            StreamKey::new("edge", "live", "camera"),
            session_id,
            vec![(
                RecorderDefinition::automatic(Some("archive".to_owned())),
                Arc::clone(&controller),
            )],
            Vec::new(),
            1_000,
        )
        .expect("managed publisher");
    let stream_id = registration.stream_id();
    let recorder_id = registration.recorder_ids()[0];
    registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);

    assert_continuous_catalog_recovery(&registry, &controller, stream_id, session_id, recorder_id);
    drop(registration);
    drop(reaper_owner);
}

#[test]
fn continuous_start_failure_counts_media_dropped_before_a_worker_exists() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(8),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let lease = store.acquire_recorder().expect("occupy recorder capacity");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
    let controller = test_controller_with_start(
        &store,
        &reaper,
        "camera",
        1024,
        Duration::from_secs(1),
        RtmpRecorderStart::Continuous,
    );
    let session_id = SessionId::new();
    let registration = registry
        .register_managed_publisher(
            StreamKey::new("edge", "live", "camera"),
            session_id,
            vec![(
                RecorderDefinition::automatic(Some("archive".to_owned())),
                Arc::clone(&controller),
            )],
            Vec::new(),
            1_000,
        )
        .expect("managed publisher");
    let stream_id = registration.stream_id();
    let recorder_id = registration.recorder_ids()[0];
    registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);

    let event =
        crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"dropped"[..])).expect("recovery event");
    let enqueue_result = controller.try_enqueue(event, 1_100);
    assert_eq!(enqueue_result, RecorderEnqueueResult::Inactive);
    assert!(controller.status().status.is_none());
    registry.update_recorder_runtime(stream_id, session_id, recorder_id, enqueue_result, 1_100);

    assert_eq!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("failed recorder snapshot")
            .events_dropped,
        1
    );
    drop(lease);
    drop(registration);
    drop(reaper_owner);
}

#[test]
fn proactive_shutdown_transitions_stalled_recording_from_stopping_to_failed() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024),
            max_files: Some(2),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let ownership = OpenOptions::new()
        .read(true)
        .open(root.path())
        .expect("recording root ownership");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
    let controller = test_controller(&store, &reaper, "camera", 1024, Duration::from_secs(1));
    let session_id = SessionId::new();
    let registration = registry
        .register_managed_publisher(
            StreamKey::new("edge", "live", "camera"),
            session_id,
            vec![(
                RecorderDefinition::automatic(Some("archive".to_owned())),
                Arc::clone(&controller),
            )],
            Vec::new(),
            1_000,
        )
        .expect("managed publisher");
    let stream_id = registration.stream_id();
    let recorder_id = registration.recorder_ids()[0];
    registry.start_continuous_recording(stream_id, session_id, recorder_id, 1_000);
    flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
    assert_eq!(
        controller.try_enqueue(
            crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
            1_100,
        ),
        RecorderEnqueueResult::Queued
    );
    thread::sleep(Duration::from_millis(20));

    let shutdown_deadline = Instant::now() + Duration::from_millis(50);
    let shutdown = reaper_owner.initiate_shutdown(shutdown_deadline);
    registry.initiate_recorder_shutdown(&reaper);
    assert!(matches!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("stopping snapshot")
            .phase,
        RecorderPhase::Stopping { .. }
    ));
    assert!(shutdown.wait_until(Instant::now() + Duration::from_secs(1)));
    assert!(matches!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("failed snapshot")
            .phase,
        RecorderPhase::Failed {
            code: RecorderErrorCode::ShutdownTimedOut,
            ..
        }
    ));
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
    drop(registration);
}

#[test]
fn catalog_start_after_shutdown_snapshot_fails_terminally() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024),
            max_files: Some(2),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (reaper_owner, reaper) = registry.create_recorder_reaper(1);
    let shutdown = reaper_owner.initiate_shutdown(Instant::now() + Duration::from_secs(1));
    let controller = test_controller(&store, &reaper, "camera", 1024, Duration::from_secs(1));
    let session_id = SessionId::new();
    let registration = registry
        .register_managed_publisher(
            StreamKey::new("edge", "live", "camera"),
            session_id,
            vec![(
                RecorderDefinition::manual(Some("archive".to_owned())),
                controller,
            )],
            Vec::new(),
            1_000,
        )
        .expect("managed publisher");
    let stream_id = registration.stream_id();
    let recorder_id = registration.recorder_ids()[0];

    assert!(matches!(
        registry.start_recording(stream_id, recorder_id, 1_100),
        Err(CatalogError::RecorderFailed {
            code: RecorderErrorCode::BackendUnavailable,
            ..
        })
    ));
    assert!(matches!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("failed recorder snapshot")
            .phase,
        RecorderPhase::Failed {
            code: RecorderErrorCode::BackendUnavailable,
            ..
        }
    ));
    assert_eq!(store.stats().active_recorders, 0);
    assert!(shutdown.wait_until(Instant::now() + Duration::from_secs(1)));
    drop(registration);
}

fn assert_continuous_catalog_recovery(
    registry: &Arc<RtmpRegistry>,
    controller: &Arc<RecorderController>,
    stream_id: StreamId,
    session_id: SessionId,
    recorder_id: RecorderId,
) {
    controller.fail_before_process();
    let failed_event =
        crate::MediaEvent::audio(0, Arc::<[u8]>::from(&b"failure"[..])).expect("failed event");
    assert_eq!(
        controller.try_enqueue(failed_event, 1_100),
        RecorderEnqueueResult::Queued
    );
    wait_until(Duration::from_secs(1), || {
        controller.status().status.is_some_and(|status| {
            status.phase == RecorderWorkerPhase::Failed(RecorderFailure::Open)
        })
    });
    registry.update_recorder_runtime(
        stream_id,
        session_id,
        recorder_id,
        RecorderEnqueueResult::Inactive,
        1_100,
    );
    assert!(matches!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("failed recorder snapshot")
            .phase,
        RecorderPhase::Failed { .. }
    ));
    assert_stale_recording_status_does_not_hide_failure(
        registry,
        controller,
        stream_id,
        recorder_id,
    );

    let recovery_event =
        crate::MediaEvent::audio(1, Arc::<[u8]>::from(&b"recover"[..])).expect("recovery event");
    assert_eq!(
        controller.try_enqueue(recovery_event, 1_200),
        RecorderEnqueueResult::Inactive
    );
    wait_until(Duration::from_secs(1), || {
        let status = controller.status();
        !status.stopping && status.recovering
    });
    thread::sleep(Duration::from_millis(20));
    let continued_event =
        crate::MediaEvent::audio(2, Arc::<[u8]>::from(&b"continued"[..])).expect("continued event");
    let enqueue_result = controller.try_enqueue(continued_event, 1_300);
    assert_eq!(enqueue_result, RecorderEnqueueResult::Queued);
    wait_until(Duration::from_secs(1), || {
        controller
            .status()
            .status
            .is_some_and(|status| status.phase == RecorderWorkerPhase::Recording)
    });
    registry.update_recorder_runtime(stream_id, session_id, recorder_id, enqueue_result, 1_300);
    assert!(matches!(
        registry
            .recorder_snapshot(stream_id, recorder_id)
            .expect("recovered recorder snapshot")
            .phase,
        RecorderPhase::Recording { .. }
    ));

    controller.deactivate(1_400);
    wait_until(Duration::from_secs(1), || !controller.status().stopping);
}

fn assert_stale_recording_status_does_not_hide_failure(
    registry: &RtmpRegistry,
    controller: &RecorderController,
    stream_id: StreamId,
    recorder_id: RecorderId,
) {
    let mut stale_runtime = controller.status();
    stale_runtime.stopping = false;
    stale_runtime.recovering = false;
    stale_runtime
        .status
        .as_mut()
        .expect("failed runtime status")
        .phase = RecorderWorkerPhase::Recording;
    let mut inner = registry.lock();
    let recorder = inner
        .streams
        .get_mut(&stream_id)
        .expect("managed stream")
        .recorders
        .get_mut(&recorder_id)
        .expect("managed recorder");
    apply_runtime_status(recorder, stale_runtime);
    assert!(matches!(recorder.phase, RecorderPhase::Failed { .. }));
}

fn test_controller(
    store: &RecordingStore,
    reaper: &RecorderReaperHandle,
    name: &'static str,
    max_queue_bytes: usize,
    shutdown_timeout: Duration,
) -> Arc<RecorderController> {
    test_controller_with_start(
        store,
        reaper,
        name,
        max_queue_bytes,
        shutdown_timeout,
        RtmpRecorderStart::Manual,
    )
}

fn test_controller_with_start(
    store: &RecordingStore,
    reaper: &RecorderReaperHandle,
    name: &'static str,
    max_queue_bytes: usize,
    shutdown_timeout: Duration,
    start: RtmpRecorderStart,
) -> Arc<RecorderController> {
    Arc::new(RecorderController::new(
        RtmpRecorderPolicy::new(
            "archive",
            start,
            store.clone(),
            RecordingPathPolicy::new(".flv", false).expect("recording path policy"),
            RecorderWorkerConfig {
                max_queue_messages: 4,
                max_queue_bytes,
                rotation_interval: None,
                shutdown_timeout,
                video_codec: None,
                ..RecorderWorkerConfig::default()
            },
        ),
        Arc::<[u8]>::from(name.as_bytes()),
        reaper.clone(),
        1_000,
    ))
}

fn recorder_context() -> RecorderCommandContext {
    RecorderCommandContext {
        stream_id: StreamId::new(),
        publisher_session_id: SessionId::new(),
        recorder_id: RecorderId::new(),
        operation_id: OperationId::new(),
        at_unix_ms: 1_000,
    }
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timeout");
        thread::sleep(Duration::from_millis(2));
    }
}
