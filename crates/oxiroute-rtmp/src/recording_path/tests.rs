
use super::*;

#[test]
fn recognizes_safe_sequence_and_collision_variants_for_resume() {
    let policy = RecordingPathPolicy::new(".flv", true).expect("recording policy");

    assert_eq!(
        policy.segment_identity_from_filename(b"camera", "camera-100.flv"),
        Some((100, 0, 0))
    );
    assert_eq!(
        policy.segment_identity_from_filename(b"camera", "camera-100-2.flv"),
        Some((100, 0, 2))
    );
    assert_eq!(
        policy.segment_identity_from_filename(b"camera", "camera-100-000001.flv"),
        Some((100, 1, 0))
    );
    assert_eq!(
        policy.segment_identity_from_filename(b"camera", "camera-100-000001-2.flv"),
        Some((100, 1, 2))
    );
}

#[test]
fn recognizes_only_collision_variants_for_nginx_naming() {
    let policy = RecordingPathPolicy::new(".flv", true)
        .expect("recording policy")
        .with_segment_policy(
            RecordingTimezone::Utc,
            RecordingTimeBasis::SegmentStart,
            RecordingSegmentNaming::NginxCompatible,
        );

    assert_eq!(
        policy.segment_identity_from_filename(b"camera", "camera-100-3.flv"),
        Some((100, 0, 3))
    );
    assert_eq!(
        policy.segment_identity_from_filename(b"camera", "camera-100-000001.flv"),
        None
    );
}

#[test]
fn preserves_identity_when_suffixing_a_maximum_length_filename() {
    let policy = RecordingPathPolicy::new(".flv", true).expect("recording policy");
    let stream_name = vec![b'a'; 247];
    let base = policy
        .relative_filename_at(&stream_name, 100)
        .expect("maximum length base");
    assert_eq!(base.len(), MAX_RECORDING_FILENAME_BYTES);

    let sequenced = policy
        .segment_filename(
            &stream_name,
            100,
            RecordingDateTime::from_unix_seconds(100).expect("date time"),
            1,
        )
        .expect("sequenced filename");
    assert!(sequenced.len() <= MAX_RECORDING_FILENAME_BYTES - 3);
    assert_eq!(
        policy.segment_identity_from_filename(&stream_name, &sequenced),
        Some((100, 1, 0))
    );

    let collided = collision_recording_filename(&sequenced, 15).expect("collision filename");
    assert_eq!(collided.len(), MAX_RECORDING_FILENAME_BYTES);
    assert_eq!(
        policy.segment_identity_from_filename(&stream_name, &collided),
        Some((100, 1, 15))
    );
}

#[test]
fn compact_segment_identities_do_not_alias_streams_or_long_suffixes() {
    let policy = RecordingPathPolicy::new(&format!("{}.flv", "x".repeat(70)), true)
        .expect("recording policy");
    let mut first_stream = vec![b'a'; 177];
    let mut second_stream = first_stream.clone();
    first_stream[80] = b'b';
    second_stream[80] = b'c';
    let opened_at = RecordingDateTime::from_unix_seconds(100).expect("date time");
    let first = policy
        .segment_filename(&first_stream, 100, opened_at, 1)
        .expect("first filename");
    let second = policy
        .segment_filename(&second_stream, 100, opened_at, 1)
        .expect("second filename");

    assert_ne!(first, second);
    assert_eq!(
        policy.segment_identity_from_filename(&first_stream, &first),
        Some((100, 1, 0))
    );
    assert_eq!(
        policy.segment_identity_from_filename(&second_stream, &first),
        None
    );
}

#[test]
fn recognizes_native_seconds_before_a_digit_prefixed_suffix() {
    let policy = RecordingPathPolicy::new("1.flv", true).expect("recording policy");
    assert_eq!(
        policy.segment_identity_from_filename(b"camera", "camera-1001.flv"),
        Some((100, 0, 0))
    );
}

#[test]
fn compact_identity_distinguishes_a_numeric_suffix_from_a_collision() {
    let policy = RecordingPathPolicy::new("-2.flv", true).expect("recording policy");
    let stream_name = vec![b'a'; 245];
    let filename = policy
        .segment_filename(
            &stream_name,
            100,
            RecordingDateTime::from_unix_seconds(100).expect("date time"),
            1,
        )
        .expect("compact filename");
    assert_eq!(
        policy.segment_identity_from_filename(&stream_name, &filename),
        Some((100, 1, 0))
    );

    let collision = collision_recording_filename(&filename, 2).expect("collision filename");
    assert_eq!(
        policy.segment_identity_from_filename(&stream_name, &collision),
        Some((100, 1, 2))
    );
}
