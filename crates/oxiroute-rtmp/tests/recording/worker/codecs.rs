use std::fs;

use oxiroute_rtmp::{
    MediaEvent, RecorderVideoCodec, RecorderWorker, RecorderWorkerConfig, RecorderWorkerPhase,
    RecordingDateTime, RecordingPathPolicy,
};
use tempfile::tempdir;

use super::common::{enqueue, parse_tags, shutdown, store, worker};

fn enhanced_video_header(four_cc: [u8; 4], marker: u8) -> MediaEvent {
    MediaEvent::video(0, enhanced_payload(0x90, four_cc, marker)).expect("enhanced video header")
}

fn enhanced_video_keyframe(four_cc: [u8; 4], marker: u8) -> MediaEvent {
    MediaEvent::video(1, enhanced_payload(0x91, four_cc, marker)).expect("enhanced video keyframe")
}

fn enhanced_video_interframe(four_cc: [u8; 4], marker: u8) -> MediaEvent {
    MediaEvent::video(2, enhanced_payload(0xa1, four_cc, marker))
        .expect("enhanced video interframe")
}

fn enhanced_payload(header: u8, four_cc: [u8; 4], marker: u8) -> Vec<u8> {
    let mut payload = vec![header];
    payload.extend_from_slice(&four_cc);
    payload.push(marker);
    payload
}

#[test]
fn enhanced_codec_event_is_accounted_as_processed() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let worker = worker(&store, None, 1024);
    enqueue(&worker, enhanced_video_header(*b"hvc1", 0x01));

    let status = shutdown(worker);
    assert_eq!(status.events_enqueued, 1);
    assert_eq!(status.events_processed, 1);
    assert_eq!(status.events_dropped, 0);
    assert_eq!(status.queue_messages, 0);
    assert_eq!(status.queue_bytes, 0);
}

#[test]
fn accepts_declared_video_codecs_before_starting_a_worker() {
    let temporary = tempdir().expect("temporary directory");
    let store = store(temporary.path());
    for codec in [
        RecorderVideoCodec::EnhancedAvc,
        RecorderVideoCodec::Hevc,
        RecorderVideoCodec::Av1,
    ] {
        let worker = RecorderWorker::start(
            store.clone(),
            &RecordingPathPolicy::new(".flv", false).expect("path policy"),
            b"camera",
            1_721_657_969,
            RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
            RecorderWorkerConfig {
                video_codec: Some(codec),
                ..RecorderWorkerConfig::default()
            },
        )
        .expect("declared codec is recordable");
        assert_eq!(shutdown(worker).phase, RecorderWorkerPhase::Stopped);
    }
    assert_eq!(store.stats().files, 0);
}

#[test]
fn records_enhanced_avc_hevc_and_av1_as_enhanced_flv() {
    for four_cc in [*b"avc1", *b"hvc1", *b"av01"] {
        let temporary = tempdir().expect("temporary directory");
        let store = store(temporary.path());
        let worker = worker(&store, None, 1024);
        enqueue(&worker, enhanced_video_header(four_cc, 0x01));
        enqueue(&worker, enhanced_video_keyframe(four_cc, 0x02));
        enqueue(&worker, enhanced_video_interframe(four_cc, 0x03));

        let status = shutdown(worker);
        assert_eq!(status.phase, RecorderWorkerPhase::Stopped);
        assert_eq!(status.segments_started, 1);
        assert_eq!(status.segments_completed, 1);
        let tags =
            parse_tags(&fs::read(temporary.path().join("camera.flv")).expect("recorded FLV"));
        assert_eq!(
            tags.iter().map(|(tag, _, _)| *tag).collect::<Vec<_>>(),
            [9, 9, 9]
        );
        assert_eq!(tags[0].2, enhanced_payload(0x90, four_cc, 0x01));
        assert_eq!(tags[1].2, enhanced_payload(0x91, four_cc, 0x02));
        assert_eq!(tags[2].2, enhanced_payload(0xa1, four_cc, 0x03));
    }
}
