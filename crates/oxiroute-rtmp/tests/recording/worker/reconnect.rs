use std::fs;
use std::time::Duration;

use oxiroute_rtmp::{
    MediaEvent, RecorderEnqueueResult, RecorderFailure, RecorderWorker, RecorderWorkerConfig,
    RecorderWorkerPhase, RecordingDateTime, RecordingPathPolicy, RecordingSegmentNaming,
    RecordingTimeBasis, RecordingTimezone,
};
use tempfile::tempdir;

use super::common::{
    aac_header, audio, audio_payload, enqueue, parse_tags, payloads, recording_files, shutdown,
    store, wait_for_processed,
};

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
        rotation_interval: Some(Duration::from_hours(1)),
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
        rotation_interval: Some(Duration::from_hours(1)),
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
    wait_for_processed(&first, 2);
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
        rotation_interval: Some(Duration::from_hours(1)),
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
        rotation_interval: Some(Duration::from_mins(1)),
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
