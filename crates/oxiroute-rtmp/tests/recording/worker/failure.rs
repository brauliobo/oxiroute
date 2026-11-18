use std::fs::OpenOptions;
use std::time::{Duration, Instant};
use std::{fs, thread};

use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MediaEvent, RecorderEnqueueResult, RecorderFailure, RecorderShutdown,
    RecorderWorker, RecorderWorkerConfig, RecorderWorkerPhase, RecordingDateTime,
    RecordingPathPolicy, RecordingSegmentNaming, RecordingStore, RecordingStoreLimits,
    RecordingTimeBasis, RecordingTimezone, StreamKey,
};
use rustix::fs::{FlockOperation, flock};
use tempfile::tempdir;

use super::common::{
    aac_header, audio, enqueue, recording_files, shutdown, store, wait_for_processed, worker,
    worker_with_config,
};

fn worker_with_shutdown_timeout(
    store: &RecordingStore,
    shutdown_timeout: Duration,
) -> RecorderWorker {
    RecorderWorker::start(
        store.clone(),
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        RecorderWorkerConfig {
            max_queue_messages: 32,
            max_queue_bytes: 1024,
            rotation_interval: None,
            shutdown_timeout,
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker")
}

fn wait_for_failure(worker: &RecorderWorker) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !matches!(worker.status().phase, RecorderWorkerPhase::Failed(_)) {
        assert!(Instant::now() < deadline, "recorder failure timeout");
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_recorders_to_stop(store: &RecordingStore) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while store.stats().active_recorders != 0 {
        assert!(Instant::now() < deadline, "recorder cancellation timeout");
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(20));
}

fn legacy_audio(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, vec![0x2f, marker]).expect("legacy audio")
}

#[test]
fn bounded_try_enqueue_drops_an_oversized_event_without_opening_storage() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker(&store, None, 2);
    assert_eq!(
        worker.try_enqueue(audio(0, 0x11)),
        RecorderEnqueueResult::DroppedDiscontinuity
    );

    let status = shutdown(worker);
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::Discontinuity)
    );
    assert_eq!(status.events_dropped, 1);
    assert_eq!(status.discontinuities, 1);
    assert_eq!(status.events_enqueued, 0);
    assert_eq!(status.segments_started, 0);
    assert_eq!(store.stats().files, 0);
}

#[test]
fn a_queue_drop_quarantines_the_active_segment_and_stops_continuation() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker_with_config(
        &store,
        RecorderWorkerConfig {
            max_queue_messages: 8,
            max_queue_bytes: 4,
            rotation_interval: None,
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    );
    enqueue(&worker, aac_header(0, 0x12));
    wait_for_processed(&worker, 1);
    let oversized = MediaEvent::audio(1, vec![0x2f, 1, 2, 3, 4]).expect("oversized event");
    assert_eq!(
        worker.try_enqueue(oversized),
        RecorderEnqueueResult::DroppedDiscontinuity
    );

    let status = shutdown(worker);
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::Discontinuity)
    );
    assert_eq!(status.discontinuities, 1);
    assert_eq!(status.segments_completed, 0);
    let partial = status
        .recoverable_partial_name
        .expect("quarantined discontinuous segment");
    assert_eq!(partial, "camera.flv");
    assert!(temporary.path().join(partial).is_file());
}

#[test]
fn simulated_full_disk_fails_only_the_recorder_and_leaves_live_playback_healthy() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(13),
            max_files: Some(4),
            max_active_recorders: 1,
            retention: oxiroute_rtmp::RecordingRetention::default(),
        },
    )
    .expect("recording store");
    let worker = worker(&store, None, 1024);
    let event = legacy_audio(10, 0x66);
    enqueue(&worker, event.clone());
    wait_for_failure(&worker);

    let hub = LiveHub::new(LiveHubLimits::default());
    let viewer = hub
        .subscribe(StreamKey::new("edge", "live", "camera"))
        .expect("viewer");
    let publisher = hub
        .attach_publisher(StreamKey::new("edge", "live", "camera"))
        .expect("publisher");
    publisher.publish(event.clone()).expect("live publication");
    assert_eq!(viewer.try_next(), Some(event));
    assert_eq!(
        worker.try_enqueue(legacy_audio(20, 0x77)),
        RecorderEnqueueResult::Inactive
    );

    let status = shutdown(worker);
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::QuotaBytes)
    );
    assert_eq!(status.bytes_written, 13);
    assert_eq!(status.segments_completed, 0);
    assert_eq!(store.stats().files, 1);
    let partial = status
        .recoverable_partial_name
        .expect("recoverable write-failure partial");
    assert!(temporary.path().join(partial).is_file());
}

#[test]
fn final_name_exhaustion_is_failed_without_creating_an_extra_recording() {
    let temporary = tempdir().expect("temporary directory");
    fs::write(temporary.path().join("camera.flv"), b"existing").expect("base collision");
    for suffix in 1..16 {
        fs::write(
            temporary.path().join(format!("camera-{suffix}.flv")),
            b"existing",
        )
        .expect("deterministic collision");
    }
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(64),
            max_active_recorders: 2,
            retention: oxiroute_rtmp::RecordingRetention::default(),
        },
    )
    .expect("recording store");
    let worker = worker(&store, None, 1024);
    enqueue(&worker, audio(0, 0x11));

    let status = shutdown(worker);
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::Publish)
    );
    assert_eq!(status.segments_completed, 0);
    assert_eq!(status.recoverable_partial_name, None);
    assert_eq!(recording_files(temporary.path()).len(), 16);
}

#[test]
fn segment_end_render_failure_preserves_and_reports_the_existing_recording() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let path = RecordingPathPolicy::new("-%Y.flv", false)
        .expect("path policy")
        .with_segment_policy(
            RecordingTimezone::Utc,
            RecordingTimeBasis::SegmentEnd,
            RecordingSegmentNaming::NginxCompatible,
        );
    let worker = RecorderWorker::start(
        store,
        &path,
        b"camera",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        RecorderWorkerConfig::default(),
    )
    .expect("recorder worker");
    assert_eq!(
        worker.try_enqueue_at(audio(0, 0x11), Instant::now(), 1_721_657_969_000),
        RecorderEnqueueResult::Queued
    );
    assert_eq!(
        worker.try_enqueue_at(audio(1, 0x22), Instant::now(), u64::MAX),
        RecorderEnqueueResult::Queued
    );

    let status = shutdown(worker);

    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::Finalize)
    );
    let recording = status
        .recoverable_partial_name
        .expect("render failure preserves the recording");
    assert_eq!(recording, "camera-2024.flv");
    assert!(temporary.path().join(recording).is_file());
}

#[test]
fn shutdown_is_bounded_when_storage_admission_is_stalled() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let ownership = OpenOptions::new()
        .read(true)
        .open(temporary.path())
        .expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall storage admission");
    let worker = worker_with_shutdown_timeout(&store, Duration::from_millis(40));
    enqueue(&worker, audio(0, 0x11));
    thread::sleep(Duration::from_millis(20));

    let started = Instant::now();
    let shutdown = worker.shutdown();
    assert!(started.elapsed() < Duration::from_millis(500));
    let supervisor = match shutdown {
        RecorderShutdown::TimedOut(supervisor) => supervisor,
        RecorderShutdown::Joined(status) => panic!("worker unexpectedly joined: {status:?}"),
    };
    let status = supervisor.status();
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
    );

    flock(&ownership, FlockOperation::Unlock).expect("release storage admission");
    let joined = supervisor.join();
    assert_eq!(joined.phase, status.phase);
    assert!(recording_files(temporary.path()).is_empty());
}

#[test]
fn abrupt_drop_is_bounded_when_storage_is_stalled() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let ownership = OpenOptions::new()
        .read(true)
        .open(temporary.path())
        .expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall storage admission");
    let worker = worker_with_shutdown_timeout(&store, Duration::from_millis(40));
    enqueue(&worker, audio(0, 0x11));
    thread::sleep(Duration::from_millis(20));

    let started = Instant::now();
    drop(worker);
    assert!(started.elapsed() < Duration::from_millis(500));

    flock(&ownership, FlockOperation::Unlock).expect("release storage admission");
    wait_for_recorders_to_stop(&store);
    assert!(recording_files(temporary.path()).is_empty());
}

#[test]
fn status_contains_no_absolute_root_or_raw_io_error() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker(&store, None, 1024);
    enqueue(&worker, audio(0, 0x11));
    let status = shutdown(worker);

    assert_eq!(status.current_relative_name, None);
    assert_eq!(
        status.last_completed_relative_name.as_deref(),
        Some("camera.flv")
    );
    let status = format!("{status:?}");
    assert!(!status.contains(&temporary.path().display().to_string()));
    assert!(!status.contains("secret"));
}
