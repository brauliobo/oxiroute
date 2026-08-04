use std::{
    fs,
    fs::OpenOptions,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MediaEvent, RecorderEnqueueResult, RecorderFailure, RecorderShutdown,
    RecorderVideoCodec, RecorderWorker, RecorderWorkerConfig, RecorderWorkerPhase,
    RecorderWorkerStartError, RecordingDateTime, RecordingPathPolicy, RecordingSegmentNaming,
    RecordingStore, RecordingStoreError, RecordingStoreLimits, RecordingTimeBasis,
    RecordingTimezone, StreamKey,
};
use rustix::fs::{FlockOperation, flock};
use tempfile::tempdir;

#[test]
fn worker_output_matches_the_audio_only_flv_golden_bytes() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker(&store, None, 1024 * 1024);
    enqueue(&worker, aac_header(900, 0x12));
    enqueue(&worker, audio(1_000, 0x11));
    enqueue(&worker, audio(1_023, 0x33));

    let status = shutdown(worker);
    let output = fs::read(temporary.path().join("camera.flv")).expect("recorded FLV");
    assert_eq!(
        output,
        vec![
            0x46, 0x4c, 0x56, 0x01, 0x04, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00, 0x08,
            0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaf, 0x00, 0x12, 0x10,
            0x00, 0x00, 0x00, 0x0f, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xaf, 0x01, 0x11, 0x00, 0x00, 0x00, 0x0e, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00,
            0x17, 0x00, 0x00, 0x00, 0x00, 0xaf, 0x01, 0x33, 0x00, 0x00, 0x00, 0x0e,
        ]
    );
    assert_eq!(status.phase, RecorderWorkerPhase::Stopped);
    assert_eq!(
        status.last_completed_relative_name.as_deref(),
        Some("camera.flv")
    );
    assert_eq!(status.bytes_written, output.len() as u64);
    assert_eq!(status.events_enqueued, 3);
    assert_eq!(status.events_processed, 3);
    assert_eq!(status.events_dropped, 0);
    assert_eq!(status.queue_messages, 0);
    assert_eq!(status.queue_bytes, 0);
    assert_eq!(status.segments_started, 1);
    assert_eq!(status.segments_completed, 1);
}

#[test]
fn rotates_audio_only_on_an_audio_frame_and_replays_the_latest_codec_header() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker(&store, Some(Duration::from_millis(100)), 1024 * 1024);
    enqueue(&worker, aac_header(0, 0x12));
    enqueue(&worker, audio(0, 0x11));
    enqueue(&worker, aac_header(50, 0x13));
    enqueue(&worker, audio(99, 0x22));
    enqueue(&worker, audio(100, 0x33));

    let status = shutdown(worker);
    assert_eq!(status.segments_completed, 2);
    let first = parse_tags(&fs::read(temporary.path().join("camera.flv")).expect("first segment"));
    let second_path = temporary.path().join("camera-000001.flv");
    let second = parse_tags(&fs::read(second_path).expect("second segment"));

    assert_eq!(
        payloads(&first),
        vec![
            aac_payload(0x12),
            audio_payload(0x11),
            aac_payload(0x13),
            audio_payload(0x22)
        ]
    );
    assert_eq!(
        payloads(&second),
        vec![aac_payload(0x13), audio_payload(0x33)]
    );
    assert_eq!(second[0].1, 0);
    assert_eq!(second[1].1, 0);
}

#[test]
fn delays_video_rotation_until_a_keyframe_and_replays_codec_headers() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker(&store, Some(Duration::from_millis(100)), 1024 * 1024);
    enqueue(&worker, aac_header(0, 0x12));
    enqueue(&worker, avc_header(0, 0x01));
    enqueue(&worker, keyframe(0, 0x11));
    enqueue(&worker, audio(50, 0x44));
    enqueue(&worker, interframe(100, 0x22));
    enqueue(&worker, keyframe(150, 0x33));
    enqueue(&worker, audio(160, 0x55));

    let status = shutdown(worker);
    assert_eq!(status.segments_completed, 2);
    let first = parse_tags(&fs::read(temporary.path().join("camera.flv")).expect("first segment"));
    let second_path = temporary.path().join("camera-000001.flv");
    let second = parse_tags(&fs::read(second_path).expect("second segment"));

    assert!(payloads(&first).contains(&video_payload(0x27, 0x22)));
    assert_eq!(
        payloads(&second),
        vec![
            aac_payload(0x12),
            avc_header_payload(0x01),
            video_payload(0x17, 0x33),
            audio_payload(0x55),
        ]
    );
    assert_eq!(
        second.iter().map(|tag| tag.1).collect::<Vec<_>>(),
        vec![0, 0, 0, 10]
    );
}

#[test]
fn every_rotation_gets_a_fresh_deterministic_extension_preserving_name() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(64),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let worker = worker_with_config(
        &store,
        RecorderWorkerConfig {
            max_queue_messages: 64,
            max_queue_bytes: 1024 * 1024,
            rotation_interval: Some(Duration::from_millis(1)),
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    );
    enqueue(&worker, aac_header(0, 0x12));
    for timestamp in 0_u8..=20 {
        enqueue(&worker, audio(u32::from(timestamp), timestamp));
        if timestamp == 0 {
            continue;
        }
        let completed_sequence = timestamp - 1;
        let completed_name = if completed_sequence == 0 {
            "camera.flv".to_owned()
        } else {
            format!("camera-{completed_sequence:06}.flv")
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !temporary.path().join(&completed_name).is_file() {
            assert!(
                Instant::now() < deadline,
                "segment publication timeout: {completed_name}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    let status = shutdown(worker);
    assert_eq!(status.segments_completed, 21);
    assert!(temporary.path().join("camera.flv").is_file());
    for sequence in 1..=20 {
        assert!(
            temporary
                .path()
                .join(format!("camera-{sequence:06}.flv"))
                .is_file()
        );
    }
}

#[test]
fn recorder_capacity_is_held_for_the_worker_lifetime_across_rotations() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(16),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let first = worker(&store, Some(Duration::from_millis(1)), 1024 * 1024);
    assert_eq!(store.stats().active_recorders, 1);

    let second = RecorderWorker::start(
        store.clone(),
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"second",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        RecorderWorkerConfig::default(),
    );
    assert!(matches!(
        second,
        Err(RecorderWorkerStartError::Capacity(
            RecordingStoreError::ActiveRecorderLimit { maximum: 1 }
        ))
    ));

    enqueue(&first, audio(0, 0x11));
    enqueue(&first, audio(1, 0x22));
    let status = shutdown(first);
    assert_eq!(status.segments_completed, 2);
    assert_eq!(store.stats().active_recorders, 0);

    let replacement = worker(&store, None, 1024);
    assert_eq!(store.stats().active_recorders, 1);
    shutdown(replacement);
    assert_eq!(store.stats().active_recorders, 0);
}

#[test]
fn two_recorders_rotate_through_the_shared_root_finalizer() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(16),
            max_active_recorders: 2,
        },
    )
    .expect("recording store");
    let path = RecordingPathPolicy::new(".flv", false).expect("path policy");
    let config = RecorderWorkerConfig {
        rotation_interval: Some(Duration::from_millis(1)),
        ..RecorderWorkerConfig::default()
    };
    let first = RecorderWorker::start(
        store.clone(),
        &path,
        b"camera",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        config,
    )
    .expect("first recorder");
    let second = RecorderWorker::start(
        store.clone(),
        &path,
        b"door",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        config,
    )
    .expect("second recorder");
    assert_eq!(store.stats().active_recorders, 2);

    for worker in [&first, &second] {
        enqueue(worker, audio(0, 0x11));
        enqueue(worker, audio(1, 0x22));
    }
    let first = shutdown(first);
    let second = shutdown(second);

    assert_eq!(first.segments_completed, 2);
    assert_eq!(second.segments_completed, 2);
    assert_eq!(store.stats().active_recorders, 0);
    assert_eq!(recording_files(temporary.path()).len(), 4);
}

#[test]
fn hourly_bahia_segments_rerender_the_suffix_and_keep_flv_payload_with_mp4_names() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(16),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let path = RecordingPathPolicy::new("-%Y-%m-%d_%H.mp4", false)
        .expect("path policy")
        .with_segment_policy(
            RecordingTimezone::Iana("America/Bahia".parse().expect("IANA timezone")),
            RecordingTimeBasis::SegmentStart,
            RecordingSegmentNaming::NginxCompatible,
        );
    let worker = RecorderWorker::start(
        store,
        &path,
        b"camera",
        1_721_619_000,
        RecordingDateTime::new(2024, 7, 22, 3, 30, 0).expect("UTC start"),
        RecorderWorkerConfig {
            max_queue_messages: 16,
            max_queue_bytes: 1024,
            rotation_interval: Some(Duration::from_secs(3_600)),
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker");
    enqueue(&worker, aac_header(0, 0x12));
    enqueue(&worker, audio(0, 0x10));
    enqueue(&worker, audio(3_600_000, 0x11));
    enqueue(&worker, audio(7_200_000, 0x12));

    let status = shutdown(worker);
    assert_eq!(status.segments_completed, 3);
    for hour in 0..=2 {
        let name = format!("camera-2024-07-22_{hour:02}.mp4");
        let payload = fs::read(temporary.path().join(name)).expect("hourly segment");
        assert_eq!(&payload[..3], b"FLV");
    }
}

#[test]
fn reconnect_within_interval_resumes_the_existing_nginx_named_segment() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let path = RecordingPathPolicy::new("-%Y%m%d_%H%M%S.mp4", true)
        .expect("path policy")
        .with_segment_policy(
            RecordingTimezone::Utc,
            RecordingTimeBasis::SegmentStart,
            RecordingSegmentNaming::NginxCompatible,
        );
    let config = RecorderWorkerConfig {
        max_queue_messages: 32,
        max_queue_bytes: 1024 * 1024,
        rotation_interval: Some(Duration::from_secs(3_600)),
        shutdown_timeout: Duration::from_secs(1),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    };
    let opened_at = 1_721_619_000;
    let first = RecorderWorker::start(
        store.clone(),
        &path,
        b"camera",
        opened_at,
        RecordingDateTime::from_unix_seconds(opened_at).expect("first start"),
        config,
    )
    .expect("first worker");
    enqueue(&first, aac_header(0, 0x12));
    enqueue(&first, audio(0, 0x11));
    enqueue(&first, audio(100, 0x22));
    let first_status = shutdown(first);
    let name = first_status
        .last_completed_relative_name
        .expect("first segment name");
    let first_length = fs::metadata(temporary.path().join(&name))
        .expect("first segment")
        .len();

    let reconnected_at = opened_at + 120;
    let second = RecorderWorker::start(
        store,
        &path,
        b"camera",
        reconnected_at,
        RecordingDateTime::from_unix_seconds(reconnected_at).expect("reconnect start"),
        config,
    )
    .expect("second worker");
    enqueue(&second, aac_header(0, 0x13));
    enqueue(&second, audio(0, 0x33));
    enqueue(&second, audio(100, 0x44));
    let second_status = shutdown(second);

    assert_eq!(recording_files(temporary.path()).len(), 1);
    assert_eq!(
        second_status.last_completed_relative_name.as_deref(),
        Some(name.as_str())
    );
    let resumed = fs::read(temporary.path().join(name)).expect("resumed segment");
    assert!(resumed.len() as u64 > first_length);
    let tags = parse_tags(&resumed);
    assert!(payloads(&tags).contains(&audio_payload(0x11)));
    assert!(payloads(&tags).contains(&audio_payload(0x44)));
    assert!(tags.windows(2).all(|tags| tags[0].1 <= tags[1].1));
}

#[test]
fn interrupted_segment_keeps_its_final_name_and_resumes_without_a_partial_file() {
    let temporary = tempdir().expect("recording root");
    let store = store(temporary.path());
    let path = RecordingPathPolicy::new(".flv", true)
        .expect("path policy")
        .with_segment_policy(
            RecordingTimezone::Utc,
            RecordingTimeBasis::SegmentStart,
            RecordingSegmentNaming::NginxCompatible,
        );
    let config = RecorderWorkerConfig {
        max_queue_messages: 32,
        max_queue_bytes: 8,
        rotation_interval: Some(Duration::from_secs(3_600)),
        shutdown_timeout: Duration::from_secs(1),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    };
    let opened_at = 1_721_619_000;
    let first = RecorderWorker::start(
        store.clone(),
        &path,
        b"camera",
        opened_at,
        RecordingDateTime::from_unix_seconds(opened_at).expect("first start"),
        config,
    )
    .expect("first worker");
    enqueue(&first, aac_header(0, 0x12));
    enqueue(&first, audio(0, 0x11));
    wait_for_recording(&first);
    assert_eq!(
        first.try_enqueue(
            MediaEvent::audio(
                1,
                vec![0xaf, 0x01, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
            )
            .expect("oversized audio")
        ),
        RecorderEnqueueResult::DroppedDiscontinuity
    );
    let first_status = shutdown(first);
    assert_eq!(
        first_status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::Discontinuity)
    );
    let first_name = first_status
        .recoverable_partial_name
        .expect("interrupted final name");
    let first_path = temporary.path().join(&first_name);
    let first_length = fs::metadata(&first_path)
        .expect("interrupted final path")
        .len();
    assert!(recording_files(temporary.path()).iter().all(|entry| {
        entry
            .extension()
            .is_none_or(|extension| extension != "partial")
    }));

    let reconnected_at = opened_at + 120;
    let second = RecorderWorker::start(
        store,
        &path,
        b"camera",
        reconnected_at,
        RecordingDateTime::from_unix_seconds(reconnected_at).expect("reconnect start"),
        config,
    )
    .expect("second worker");
    enqueue(&second, aac_header(0, 0x13));
    enqueue(&second, audio(0, 0x33));
    let second_status = shutdown(second);

    assert_eq!(
        second_status.last_completed_relative_name.as_deref(),
        Some(first_name.as_str())
    );
    assert!(fs::metadata(first_path).expect("resumed final path").len() > first_length);
    assert_eq!(recording_files(temporary.path()).len(), 1);
    assert!(recording_files(temporary.path()).iter().all(|entry| {
        entry
            .extension()
            .is_none_or(|extension| extension != "partial")
    }));
}

#[test]
fn reconnect_within_interval_preserves_a_safe_sequence_variant_name() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let path = RecordingPathPolicy::new(".flv", true).expect("path policy");
    let config = RecorderWorkerConfig {
        max_queue_messages: 32,
        max_queue_bytes: 1024 * 1024,
        rotation_interval: Some(Duration::from_secs(3_600)),
        shutdown_timeout: Duration::from_secs(1),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    };
    let opened_at = 1_721_619_000;
    let first = RecorderWorker::start(
        store.clone(),
        &path,
        b"camera",
        opened_at,
        RecordingDateTime::from_unix_seconds(opened_at).expect("first start"),
        config,
    )
    .expect("first worker");
    enqueue(&first, aac_header(0, 0x12));
    enqueue(&first, audio(0, 0x11));
    let first_status = shutdown(first);
    let base = first_status
        .last_completed_relative_name
        .expect("first segment name");
    let sequence = format!(
        "{}-000001.flv",
        base.strip_suffix(".flv").expect("FLV suffix")
    );
    fs::rename(
        temporary.path().join(&base),
        temporary.path().join(&sequence),
    )
    .expect("sequence variant");
    let first_length = fs::metadata(temporary.path().join(&sequence))
        .expect("sequence segment")
        .len();

    let reconnected_at = opened_at + 120;
    let second = RecorderWorker::start(
        store,
        &path,
        b"camera",
        reconnected_at,
        RecordingDateTime::from_unix_seconds(reconnected_at).expect("reconnect start"),
        config,
    )
    .expect("second worker");
    enqueue(&second, aac_header(0, 0x13));
    enqueue(&second, audio(0, 0x22));
    enqueue(&second, audio(3_600_000, 0x33));
    let second_status = shutdown(second);

    assert_eq!(second_status.segments_completed, 2);
    assert!(
        fs::metadata(temporary.path().join(sequence))
            .expect("resumed sequence segment")
            .len()
            > first_length
    );
    assert!(
        recording_files(temporary.path())
            .iter()
            .any(|path| path.to_string_lossy().contains("-000002.flv"))
    );
}

#[test]
fn reconnect_after_interval_starts_a_new_nginx_named_segment() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let path = RecordingPathPolicy::new("-%Y%m%d_%H%M%S.mp4", true)
        .expect("path policy")
        .with_segment_policy(
            RecordingTimezone::Utc,
            RecordingTimeBasis::SegmentStart,
            RecordingSegmentNaming::NginxCompatible,
        );
    let config = RecorderWorkerConfig {
        max_queue_messages: 32,
        max_queue_bytes: 1024 * 1024,
        rotation_interval: Some(Duration::from_secs(60)),
        shutdown_timeout: Duration::from_secs(1),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    };
    for opened_at in [1_721_619_000, 1_721_619_061] {
        let worker = RecorderWorker::start(
            store.clone(),
            &path,
            b"camera",
            opened_at,
            RecordingDateTime::from_unix_seconds(opened_at).expect("segment start"),
            config,
        )
        .expect("worker");
        enqueue(&worker, aac_header(0, 0x12));
        enqueue(&worker, audio(0, 0x11));
        shutdown(worker);
    }
    assert_eq!(recording_files(temporary.path()).len(), 2);
}

#[test]
fn record_unique_and_segment_end_are_recomputed_for_every_rotation() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let path = RecordingPathPolicy::new(".mp4", true)
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
        1_721_619_000,
        RecordingDateTime::new(2024, 7, 22, 3, 30, 0).expect("UTC start"),
        RecorderWorkerConfig {
            max_queue_messages: 8,
            max_queue_bytes: 1024,
            rotation_interval: Some(Duration::from_secs(1)),
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker");
    enqueue(&worker, aac_header(0, 0x12));
    enqueue(&worker, audio(0, 0x10));
    enqueue(&worker, audio(1_000, 0x11));

    let status = shutdown(worker);
    assert_eq!(status.segments_completed, 2);
    for second in [1_721_619_000, 1_721_619_001] {
        let payload = fs::read(temporary.path().join(format!("camera-{second}.mp4")))
            .expect("record_unique segment");
        assert_eq!(&payload[..3], b"FLV");
    }
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
fn terminal_failure_accounts_for_the_event_that_caused_it() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker(&store, None, 1024);
    enqueue(&worker, enhanced_video_header(*b"hvc1", 0x01));

    let status = shutdown(worker);
    assert_eq!(status.events_enqueued, 1);
    assert_eq!(status.events_processed, 0);
    assert_eq!(status.events_dropped, 1);
    assert_eq!(status.queue_messages, 0);
    assert_eq!(status.queue_bytes, 0);
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
    wait_for_recording(&worker);
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
        RecorderWorkerPhase::Failed(RecorderFailure::Write)
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

#[test]
fn rejects_declared_enhanced_and_unsupported_codecs_before_starting_a_worker() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    for codec in [
        RecorderVideoCodec::EnhancedAvc,
        RecorderVideoCodec::Hevc,
        RecorderVideoCodec::Av1,
    ] {
        let result = RecorderWorker::start(
            store.clone(),
            &RecordingPathPolicy::new(".flv", false).expect("path policy"),
            b"camera",
            1_721_657_969,
            RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
            RecorderWorkerConfig {
                video_codec: Some(codec),
                ..RecorderWorkerConfig::default()
            },
        );
        assert!(matches!(
            result,
            Err(RecorderWorkerStartError::UnsupportedVideoCodec(rejected)) if rejected == codec
        ));
    }
    assert_eq!(store.stats().files, 0);
}

#[test]
fn enhanced_avc_hevc_and_av1_fail_before_opening_a_segment() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    for four_cc in [*b"avc1", *b"hvc1", *b"av01"] {
        let worker = worker(&store, None, 1024);
        enqueue(&worker, enhanced_video_header(four_cc, 0x01));
        wait_for_failure(&worker);

        let status = shutdown(worker);
        assert_eq!(
            status.phase,
            RecorderWorkerPhase::Failed(RecorderFailure::UnsupportedCodec)
        );
        assert_eq!(status.segments_started, 0);
        assert_eq!(store.stats().files, 0);
    }
}

fn store(root: &Path) -> RecordingStore {
    RecordingStore::open(
        root,
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(16),
            max_active_recorders: 2,
        },
    )
    .expect("recording store")
}

fn worker(
    store: &RecordingStore,
    rotation_interval: Option<Duration>,
    max_queue_bytes: usize,
) -> RecorderWorker {
    RecorderWorker::start(
        store.clone(),
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        RecorderWorkerConfig {
            max_queue_messages: 32,
            max_queue_bytes,
            rotation_interval,
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker")
}

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

fn worker_with_config(store: &RecordingStore, config: RecorderWorkerConfig) -> RecorderWorker {
    RecorderWorker::start(
        store.clone(),
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        config,
    )
    .expect("recorder worker")
}

fn shutdown(worker: RecorderWorker) -> oxiroute_rtmp::RecorderWorkerStatus {
    match worker.shutdown() {
        RecorderShutdown::Joined(status) => status,
        RecorderShutdown::TimedOut(supervisor) => {
            panic!("recorder shutdown timed out: {:?}", supervisor.status())
        }
    }
}

fn enqueue(worker: &RecorderWorker, event: MediaEvent) {
    assert_eq!(worker.try_enqueue(event), RecorderEnqueueResult::Queued);
}

fn wait_for_failure(worker: &RecorderWorker) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !matches!(worker.status().phase, RecorderWorkerPhase::Failed(_)) {
        assert!(Instant::now() < deadline, "recorder failure timeout");
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_recording(worker: &RecorderWorker) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while worker.status().phase != RecorderWorkerPhase::Recording {
        assert!(Instant::now() < deadline, "recorder start timeout");
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_recorders_to_stop(store: &RecordingStore) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while store.stats().active_recorders != 0 {
        assert!(Instant::now() < deadline, "recorder cancellation timeout");
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(20));
}

fn recording_files(root: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(root)
        .expect("recording entries")
        .map(|entry| entry.expect("recording entry"))
        .filter(|entry| entry.file_name() != ".oxiroute-recording.lock")
        .map(|entry| entry.path())
        .collect()
}

fn aac_header(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, aac_payload(marker)).expect("AAC header")
}

fn audio(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, audio_payload(marker)).expect("AAC audio")
}

fn avc_header(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, avc_header_payload(marker)).expect("AVC header")
}

fn keyframe(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, video_payload(0x17, marker)).expect("AVC keyframe")
}

fn interframe(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, video_payload(0x27, marker)).expect("AVC interframe")
}

fn legacy_audio(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, vec![0x2f, marker]).expect("legacy audio")
}

fn aac_payload(marker: u8) -> Vec<u8> {
    vec![0xaf, 0x00, marker, 0x10]
}

fn audio_payload(marker: u8) -> Vec<u8> {
    vec![0xaf, 0x01, marker]
}

fn avc_header_payload(marker: u8) -> Vec<u8> {
    vec![0x17, 0x00, 0x00, 0x00, 0x00, marker]
}

fn video_payload(header: u8, marker: u8) -> Vec<u8> {
    vec![header, 0x01, 0x00, 0x00, 0x00, marker]
}

fn enhanced_video_header(four_cc: [u8; 4], marker: u8) -> MediaEvent {
    let mut payload = vec![0x90];
    payload.extend_from_slice(&four_cc);
    payload.push(marker);
    MediaEvent::video(0, payload).expect("enhanced video header")
}

fn parse_tags(bytes: &[u8]) -> Vec<(u8, u32, Vec<u8>)> {
    assert_eq!(&bytes[..3], b"FLV");
    let mut tags = Vec::new();
    let mut offset = 13;
    while offset < bytes.len() {
        let size = usize::from_be_bytes([
            0,
            0,
            0,
            0,
            0,
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        let timestamp = u32::from_be_bytes([
            bytes[offset + 7],
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
        ]);
        let payload_start = offset + 11;
        let payload_end = payload_start + size;
        tags.push((
            bytes[offset],
            timestamp,
            bytes[payload_start..payload_end].to_vec(),
        ));
        offset = payload_end + 4;
    }
    tags
}

fn payloads(tags: &[(u8, u32, Vec<u8>)]) -> Vec<Vec<u8>> {
    tags.iter().map(|tag| tag.2.clone()).collect()
}
