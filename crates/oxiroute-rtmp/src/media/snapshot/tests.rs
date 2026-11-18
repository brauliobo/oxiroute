use std::sync::Arc;

use rml_rtmp::sessions::StreamMetadata;

use super::*;
use crate::StreamProfile;

#[test]
fn identical_traces_produce_identical_controlled_observations() {
    let mut metadata = StreamMetadata::new();
    metadata.audio_codec_id = Some(10);
    metadata.video_codec_id = Some(7);
    let trace = [
        (MediaEvent::metadata(metadata).expect("metadata"), 100),
        (audio(30, &[0xaf, 0, 1]), 110),
        (audio(20, &[0x2f, 1]), 120),
        (video(40, &[0x17, 0, 0, 0, 0, 1]), 130),
        (video(35, &[0x91, b'h', b'v', b'c', b'1', 1]), 140),
        (video(25, &[0x93, b'a', b'v', b'0', b'1', 1]), 150),
    ];
    let mut first = MediaSnapshotAccumulator::default();
    let mut second = MediaSnapshotAccumulator::default();

    for (event, at_unix_ms) in &trace {
        first.observe(event, *at_unix_ms);
        second.observe(event, *at_unix_ms);
    }

    let expected = MediaSnapshot {
        audio: TrackSnapshot {
            flv_codec_id: Some(2),
            payload_bytes_received: 5,
            last_rtmp_timestamp_ms: Some(20),
            last_observed_at_unix_ms: Some(120),
            ..TrackSnapshot::default()
        },
        video: TrackSnapshot {
            flv_codec_id: None,
            video_codec: Some(VideoCodecIdentifier::FourCc(*b"av01")),
            payload_bytes_received: 18,
            last_rtmp_timestamp_ms: Some(25),
            last_observed_at_unix_ms: Some(150),
        },
        profile: StreamProfile::default(),
        fanout_payload_bytes_queued: 77,
    };
    assert_eq!(first, second);
    assert_eq!(first.snapshot(77), expected);
}

#[test]
fn metadata_clears_advertised_codecs_without_rewriting_media_observations() {
    let mut metadata = StreamMetadata::new();
    metadata.audio_codec_id = Some(10);
    metadata.video_codec_id = Some(7);
    let mut accumulator = MediaSnapshotAccumulator::default();
    accumulator.observe(&MediaEvent::metadata(metadata).expect("metadata"), 10);
    accumulator.observe(&audio(9, &[0xaf, 1, 1]), 20);
    accumulator.observe(&video(8, &[0x17, 1, 0, 0, 0, 1]), 30);

    accumulator.observe(
        &MediaEvent::metadata(StreamMetadata::new()).expect("empty metadata"),
        40,
    );

    let snapshot = accumulator.snapshot(0);
    assert_eq!(snapshot.audio.flv_codec_id, None);
    assert_eq!(snapshot.video.flv_codec_id, None);
    assert_eq!(snapshot.video.video_codec, None);
    assert_eq!(snapshot.audio.last_observed_at_unix_ms, Some(20));
    assert_eq!(snapshot.video.last_observed_at_unix_ms, Some(30));
}

#[test]
fn payload_byte_totals_saturate() {
    let mut accumulator = MediaSnapshotAccumulator {
        audio: TrackSnapshot {
            payload_bytes_received: u64::MAX - 1,
            ..TrackSnapshot::default()
        },
        video: TrackSnapshot {
            payload_bytes_received: u64::MAX - 1,
            ..TrackSnapshot::default()
        },
        profile: ProfileAccumulator::default(),
    };

    accumulator.observe(&audio(1, &[0xaf, 1, 1]), 2);
    accumulator.observe(&video(3, &[0x17, 1, 0, 0, 0, 1]), 4);

    let snapshot = accumulator.snapshot(0);
    assert_eq!(snapshot.audio.payload_bytes_received, u64::MAX);
    assert_eq!(snapshot.video.payload_bytes_received, u64::MAX);
}

#[test]
fn advertised_parameters_fill_the_profile_until_media_measures_them() {
    let mut metadata = StreamMetadata::new();
    metadata.video_width = Some(1280);
    metadata.video_height = Some(720);
    metadata.video_frame_rate = Some(30.0);
    metadata.video_bitrate_kbps = Some(2_500);
    metadata.audio_bitrate_kbps = Some(128);
    metadata.audio_sample_rate = Some(44_100);
    metadata.audio_channels = Some(2);
    let mut accumulator = MediaSnapshotAccumulator::default();
    accumulator.observe(&MediaEvent::metadata(metadata).expect("metadata"), 10);

    assert_eq!(
        accumulator.snapshot(0).profile,
        StreamProfile {
            video_width: Some(1280),
            video_height: Some(720),
            frame_rate_millihertz: Some(30_000),
            video_bitrate_bps: Some(2_500_000),
            audio_bitrate_bps: Some(128_000),
            keyframe_interval_ms: None,
            audio_sample_rate_hz: Some(44_100),
            audio_channels: Some(2),
        }
    );
}

#[test]
fn media_measures_frame_rate_bitrate_gop_and_aac_parameters() {
    let mut accumulator = MediaSnapshotAccumulator::default();
    accumulator.observe(&audio(0, &[0xaf, 0x00, 0x12, 0x10]), 1);
    let keyframe = [&[0x17, 1, 0, 0, 0][..], &[0; 995]].concat();
    let interframe = [&[0x27, 1, 0, 0, 0][..], &[0; 495]].concat();
    for index in 0..=60_u32 {
        let payload = if index % 30 == 0 {
            &keyframe
        } else {
            &interframe
        };
        accumulator.observe(&video(index * 50, payload), 10 + u64::from(index));
    }

    let profile = accumulator.snapshot(0).profile;

    assert_eq!(profile.audio_sample_rate_hz, Some(44_100));
    assert_eq!(profile.audio_channels, Some(2));
    assert_eq!(profile.keyframe_interval_ms, Some(1_500));
    assert_eq!(profile.frame_rate_millihertz, Some(20_000));
    assert_eq!(profile.video_bitrate_bps, Some(85_333));
    assert_eq!(profile.video_width, None);
}

fn audio(timestamp_ms: u32, payload: &[u8]) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, Arc::<[u8]>::from(payload)).expect("audio event")
}

fn video(timestamp_ms: u32, payload: &[u8]) -> MediaEvent {
    MediaEvent::video(timestamp_ms, Arc::<[u8]>::from(payload)).expect("video event")
}
