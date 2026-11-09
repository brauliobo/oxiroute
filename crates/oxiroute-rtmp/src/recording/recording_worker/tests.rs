use std::{
    fs,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use tempfile::tempdir;

use super::*;
use crate::RecordingStoreLimits;

#[test]
fn rotation_drains_media_while_the_previous_segment_is_publishing() {
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
    let worker = RecorderWorker::start(
        store.clone(),
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_619_000,
        RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
        RecorderWorkerConfig {
            max_queue_messages: 8,
            max_queue_bytes: 1024,
            rotation_interval: Some(Duration::from_millis(1)),
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker");
    enqueue(
        &worker,
        MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\0\x12"[..])).unwrap(),
    );
    enqueue(
        &worker,
        MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
    );
    wait_until(|| worker.status().events_processed == 2);
    let gate = worker.install_publication_gate();

    enqueue(
        &worker,
        MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
    );
    assert!(gate.wait_before_claim(Duration::from_secs(1)));
    wait_until(|| worker.status().segments_started == 2);
    assert_eq!(store.stats().active_recorders, 1);
    enqueue(
        &worker,
        MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x33"[..])).unwrap(),
    );
    wait_until(|| worker.status().events_processed == 4);
    assert_eq!(worker.status().discontinuities, 0);

    gate.allow_claim();
    assert!(gate.wait_after_claim(Duration::from_secs(1)));
    gate.allow_publication();
    let status = match worker.shutdown() {
        RecorderShutdown::Joined(status) => status,
        RecorderShutdown::TimedOut(supervisor) => {
            panic!("recorder shutdown timed out: {:?}", supervisor.status())
        }
    };
    assert_eq!(status.segments_completed, 2);
    assert_eq!(status.discontinuities, 0);
    assert_eq!(store.stats().active_recorders, 0);
    assert_eq!(
        fs::read_dir(root.path())
            .expect("recording entries")
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .count(),
        2
    );
}

#[test]
fn ten_aligned_recorders_drain_media_while_root_finalization_is_serialized() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(16 * 1024 * 1024),
            max_files: Some(40),
            max_active_recorders: 10,
        },
    )
    .expect("recording store");
    let path = RecordingPathPolicy::new(".flv", false).expect("path policy");
    let config = RecorderWorkerConfig {
        max_queue_messages: 64,
        max_queue_bytes: 1024 * 1024,
        rotation_interval: Some(Duration::from_millis(1)),
        shutdown_timeout: Duration::from_secs(2),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    };
    let workers: Vec<_> = (0..10)
        .map(|index| {
            RecorderWorker::start(
                store.clone(),
                &path,
                format!("camera-{index}").as_bytes(),
                1_721_619_000,
                RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
                config,
            )
            .expect("recorder worker")
        })
        .collect();
    for worker in &workers {
        enqueue(
            worker,
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
        );
        wait_until(|| worker.status().events_processed == 1);
    }
    let gate = workers[0].install_publication_gate();

    enqueue(
        &workers[0],
        MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
    );
    assert!(gate.wait_before_claim(Duration::from_secs(1)));
    for worker in &workers[1..] {
        enqueue(
            worker,
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
        );
    }
    for worker in &workers {
        wait_until(|| worker.status().segments_started == 2);
        enqueue_audio_burst(worker, 1, 16);
    }
    for worker in &workers {
        wait_until(|| worker.status().events_processed == 18);
        assert_eq!(worker.status().discontinuities, 0);
    }
    for worker in &workers {
        enqueue(
            worker,
            MediaEvent::audio(2, Arc::<[u8]>::from(&b"\xaf\x01\x44"[..])).unwrap(),
        );
        wait_until(|| worker.status().segments_started == 3);
        enqueue_audio_burst(worker, 2, 16);
    }
    for worker in &workers {
        wait_until(|| worker.status().events_processed == 35);
        assert_eq!(worker.status().discontinuities, 0);
    }

    gate.allow_claim();
    assert!(gate.wait_after_claim(Duration::from_secs(1)));
    gate.allow_publication();
    for worker in workers {
        let status = match worker.shutdown() {
            RecorderShutdown::Joined(status) => status,
            RecorderShutdown::TimedOut(supervisor) => {
                panic!("aligned recorder timed out: {:?}", supervisor.status())
            }
        };
        assert_eq!(status.segments_completed, 3);
        assert_eq!(status.discontinuities, 0);
    }
    assert_eq!(store.stats().active_recorders, 0);
}

#[test]
fn queued_finalization_cancels_without_waiting_for_the_busy_root_worker() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(12),
            max_active_recorders: 2,
        },
    )
    .expect("recording store");
    let path = RecordingPathPolicy::new(".flv", false).expect("path policy");
    let config = RecorderWorkerConfig {
        max_queue_messages: 8,
        max_queue_bytes: 1024,
        rotation_interval: Some(Duration::from_millis(1)),
        shutdown_timeout: Duration::from_millis(40),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    };
    let start = |name: &[u8]| {
        RecorderWorker::start(
            store.clone(),
            &path,
            name,
            1_721_619_000,
            RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
            config,
        )
        .expect("recorder worker")
    };
    let first = start(b"first");
    let second = start(b"second");
    for worker in [&first, &second] {
        enqueue(
            worker,
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
        );
        wait_until(|| worker.status().events_processed == 1);
    }
    let first_gate = first.install_publication_gate();

    enqueue(
        &first,
        MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x22"[..])).unwrap(),
    );
    assert!(first_gate.wait_before_claim(Duration::from_secs(1)));
    enqueue(
        &second,
        MediaEvent::audio(1, Arc::<[u8]>::from(&b"\xaf\x01\x33"[..])).unwrap(),
    );
    wait_until(|| second.status().segments_started == 2);

    let shutdown = second.shutdown();
    let supervisor = match shutdown {
        RecorderShutdown::TimedOut(supervisor) => supervisor,
        RecorderShutdown::Joined(status) => {
            panic!("queued recorder unexpectedly joined before cancellation: {status:?}")
        }
    };
    wait_until(|| supervisor.is_finished());
    let status = supervisor.join();
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
    );
    assert_eq!(store.stats().active_recorders, 1);

    first_gate.allow_claim();
    assert!(first_gate.wait_after_claim(Duration::from_secs(1)));
    first_gate.allow_publication();
    assert!(matches!(first.shutdown(), RecorderShutdown::Joined(_)));
    assert_eq!(store.stats().active_recorders, 0);
    assert_eq!(
        fs::read_dir(root.path())
            .expect("recording entries")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
            .count(),
        4
    );
    assert_eq!(
        fs::read_dir(root.path())
            .expect("recording entries")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
            .count(),
        0
    );
}

#[test]
fn third_unresolved_rotation_fails_explicitly_and_preserves_the_current_segment() {
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
    let worker = RecorderWorker::start(
        store,
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_619_000,
        RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
        RecorderWorkerConfig {
            max_queue_messages: 8,
            max_queue_bytes: 1024,
            rotation_interval: Some(Duration::from_millis(1)),
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker");
    enqueue(
        &worker,
        MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
    );
    wait_until(|| worker.status().events_processed == 1);
    let gate = worker.install_publication_gate();

    for timestamp in 1..=3 {
        enqueue(
            &worker,
            MediaEvent::audio(
                timestamp,
                Arc::<[u8]>::from(
                    vec![
                        0xaf,
                        0x01,
                        u8::try_from(timestamp).expect("small test timestamp"),
                    ]
                    .into_boxed_slice(),
                ),
            )
            .unwrap(),
        );
        if timestamp == 1 {
            assert!(gate.wait_before_claim(Duration::from_secs(1)));
        }
    }
    wait_until(|| {
        matches!(
            worker.status().phase,
            RecorderWorkerPhase::Failed(RecorderFailure::Finalize)
        )
    });
    let failed = worker.status();
    assert_eq!(failed.discontinuities, 0);
    let current = failed
        .recoverable_partial_name
        .expect("current segment remains recoverable");
    assert!(root.path().join(current).is_file());

    gate.allow_claim();
    assert!(gate.wait_after_claim(Duration::from_secs(1)));
    gate.allow_publication();
    let status = match worker.shutdown() {
        RecorderShutdown::Joined(status) => status,
        RecorderShutdown::TimedOut(supervisor) => {
            panic!("failed recorder did not join: {:?}", supervisor.status())
        }
    };
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::Finalize)
    );
}

#[test]
fn record_mask_filters_events_before_the_worker_queue() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(4),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let worker = RecorderWorker::start(
        store,
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_619_000,
        RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
        RecorderWorkerConfig {
            record_mask: RecorderMediaMask::new(false, true, false),
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker");

    let audio = MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).expect("audio event");
    assert_eq!(worker.try_enqueue(audio), RecorderEnqueueResult::Filtered);
    assert_eq!(worker.status().queue_messages, 0);

    let video = MediaEvent::video(0, Arc::<[u8]>::from(&b"\x17\x01\x00\x00\x00\x01\x02"[..]))
        .expect("video event");
    assert_eq!(worker.try_enqueue(video), RecorderEnqueueResult::Queued);
    let _ = worker.shutdown();
}

#[test]
fn invalid_recording_bounds_are_rejected_before_thread_start() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(4),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let path = RecordingPathPolicy::new(".flv", false).expect("path policy");
    let start_time = RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time");

    let invalid_mask = RecorderWorker::start(
        store.clone(),
        &path,
        b"mask",
        1_721_619_000,
        start_time,
        RecorderWorkerConfig {
            record_mask: RecorderMediaMask::new(false, false, false),
            ..RecorderWorkerConfig::default()
        },
    );
    assert!(matches!(
        invalid_mask,
        Err(RecorderWorkerStartError::InvalidRecordMask)
    ));

    let invalid_limit = RecorderWorker::start(
        store,
        &path,
        b"limit",
        1_721_619_000,
        start_time,
        RecorderWorkerConfig {
            max_frames: Some(0),
            ..RecorderWorkerConfig::default()
        },
    );
    assert!(matches!(
        invalid_limit,
        Err(RecorderWorkerStartError::InvalidRecordingLimit)
    ));
}

#[test]
fn notifications_report_the_worker_lifecycle_when_enabled() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(4),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let worker = RecorderWorker::start(
        store,
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_619_000,
        RecordingDateTime::from_unix_seconds(1_721_619_000).expect("start time"),
        RecorderWorkerConfig {
            notify: true,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker");
    enqueue(
        &worker,
        MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).unwrap(),
    );
    wait_until(|| worker.status().last_notification == Some(RecorderNotification::Started));

    let status = match worker.shutdown() {
        RecorderShutdown::Joined(status) => status,
        RecorderShutdown::TimedOut(supervisor) => {
            panic!("notifying recorder timed out: {:?}", supervisor.status())
        }
    };
    assert_eq!(
        status.last_notification,
        Some(RecorderNotification::Stopped)
    );
}

fn enqueue(worker: &RecorderWorker, event: MediaEvent) {
    assert_eq!(worker.try_enqueue(event), RecorderEnqueueResult::Queued);
}

fn enqueue_audio_burst(worker: &RecorderWorker, timestamp_ms: u32, count: u8) {
    for payload in 0..count {
        enqueue(
            worker,
            MediaEvent::audio(
                timestamp_ms,
                Arc::<[u8]>::from(vec![0xaf, 0x01, payload].into_boxed_slice()),
            )
            .unwrap(),
        );
    }
}

fn wait_until(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate() {
        assert!(Instant::now() < deadline, "recorder condition timeout");
        thread::sleep(Duration::from_millis(5));
    }
}
