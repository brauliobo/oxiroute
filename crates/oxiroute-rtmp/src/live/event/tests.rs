use super::*;
use crate::{LiveHub, LiveHubLimits, PlaybackSubscription, StreamKey};

const FOUR_CCS: [([u8; 4], VideoCodec); 3] = [
    (*b"avc1", VideoCodec::Avc),
    (*b"hvc1", VideoCodec::Hevc),
    (*b"av01", VideoCodec::Av1),
];

fn enhanced_video(
    timestamp_ms: u32,
    frame_type: u8,
    packet_type: u8,
    four_cc: [u8; 4],
    marker: u8,
) -> Result<MediaEvent, MediaEventError> {
    let mut payload = vec![0x80 | frame_type << 4 | packet_type];
    payload.extend_from_slice(&four_cc);
    payload.push(marker);
    MediaEvent::video(timestamp_ms, payload)
}

fn drain_kinds(viewer: &PlaybackSubscription) -> Vec<MediaEventKind> {
    std::iter::from_fn(|| viewer.try_next())
        .map(|event| event.kind())
        .collect()
}

#[test]
fn enhanced_video_metadata_packets_are_classified_without_being_frames() {
    for (four_cc, codec) in FOUR_CCS {
        let metadata = enhanced_video(1, 5, 4, four_cc, 0x01).expect("color info metadata");
        assert_eq!(metadata.kind(), MediaEventKind::VideoMetadata);
        assert_eq!(metadata.video_codec(), Some(codec));
    }
}

#[test]
fn enhanced_video_frames_and_sequence_headers_keep_their_classification() {
    for (four_cc, codec) in FOUR_CCS {
        let sequence = enhanced_video(1, 1, 0, four_cc, 0x01).expect("sequence start");
        assert_eq!(sequence.video_codec(), Some(codec));
        assert_ne!(sequence.kind(), MediaEventKind::VideoMetadata);
        let keyframe = enhanced_video(2, 1, 1, four_cc, 0x02).expect("keyframe");
        assert_eq!(keyframe.kind(), MediaEventKind::VideoKeyframe);
        let interframe = enhanced_video(3, 2, 3, four_cc, 0x03).expect("interframe");
        assert_eq!(interframe.kind(), MediaEventKind::VideoInterframe);
    }
    assert_eq!(
        enhanced_video(4, 2, 0, *b"hvc1", 0x04),
        Err(MediaEventError::MalformedVideo)
    );
}

#[test]
fn enhanced_video_packet_types_without_a_frame_model_are_rejected_by_type() {
    for (frame_type, packet_type) in [(5, 1), (5, 0), (1, 2), (1, 5), (1, 6), (1, 7)] {
        assert_eq!(
            enhanced_video(1, frame_type, packet_type, *b"hvc1", 0x01),
            Err(MediaEventError::UnsupportedEnhancedVideoPacket(packet_type))
        );
    }
}

#[test]
fn enhanced_video_metadata_reaches_mixed_viewers_but_never_bootstraps_late_joiners() {
    let key = StreamKey::new("live", "app", "hdr");
    let hub = LiveHub::new(LiveHubLimits::default());
    let publisher = hub.attach_publisher(key.clone()).expect("publisher");
    let sequence = enhanced_video(1, 1, 0, *b"hvc1", 0x01).expect("sequence start");
    publisher.publish(sequence).expect("cache sequence header");
    let early = hub.subscribe(key.clone()).expect("early viewer");
    publisher
        .publish(enhanced_video(2, 1, 1, *b"hvc1", 0x02).expect("keyframe"))
        .expect("bootstrap keyframe");
    publisher
        .publish(enhanced_video(3, 5, 4, *b"hvc1", 0x03).expect("metadata"))
        .expect("forward metadata");
    assert_eq!(
        drain_kinds(&early),
        [
            MediaEventKind::HevcSequenceHeader,
            MediaEventKind::VideoKeyframe,
            MediaEventKind::VideoMetadata
        ]
    );

    let late = hub.subscribe(key).expect("late viewer");
    publisher
        .publish(enhanced_video(4, 5, 4, *b"hvc1", 0x04).expect("metadata"))
        .expect("gated metadata");
    assert!(late.try_next().is_none());
    publisher
        .publish(enhanced_video(5, 1, 1, *b"hvc1", 0x05).expect("keyframe"))
        .expect("bootstrap keyframe");
    assert_eq!(
        drain_kinds(&late),
        [
            MediaEventKind::HevcSequenceHeader,
            MediaEventKind::VideoKeyframe
        ]
    );
}
