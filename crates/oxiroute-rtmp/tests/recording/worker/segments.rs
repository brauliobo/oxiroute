use std::time::{Duration, Instant};
use std::{fs, thread};

use oxiroute_rtmp::{
    MediaEvent, RecorderWorker, RecorderWorkerConfig, RecorderWorkerPhase,
    RecorderWorkerStartError, RecordingDateTime, RecordingPathPolicy, RecordingSegmentNaming,
    RecordingStore, RecordingStoreError, RecordingStoreLimits, RecordingTimeBasis,
    RecordingTimezone,
};
use tempfile::tempdir;

use super::common::{
    aac_header, aac_payload, audio, audio_payload, enqueue, parse_tags, payloads, recording_files,
    shutdown, store, worker, worker_with_config,
};

fn avc_header(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, avc_header_payload(marker)).expect("AVC header")
}

fn keyframe(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, video_payload(0x17, marker)).expect("AVC keyframe")
}

fn interframe(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, video_payload(0x27, marker)).expect("AVC interframe")
}

fn avc_header_payload(marker: u8) -> Vec<u8> {
    vec![0x17, 0x00, 0x00, 0x00, 0x00, marker]
}

fn video_payload(header: u8, marker: u8) -> Vec<u8> {
    vec![header, 0x01, 0x00, 0x00, 0x00, marker]
}

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
            max_frames: Some(1),
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
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.status().segments_completed < u64::from(timestamp) {
            assert!(
                Instant::now() < deadline,
                "segment finalization timeout: {timestamp}"
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
fn hourly_zone_segments_rerender_the_suffix_and_keep_flv_payload_with_mp4_names() {
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
            RecordingTimezone::Iana(
                "America/Argentina/Buenos_Aires"
                    .parse()
                    .expect("IANA timezone"),
            ),
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
            rotation_interval: Some(Duration::from_hours(1)),
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
