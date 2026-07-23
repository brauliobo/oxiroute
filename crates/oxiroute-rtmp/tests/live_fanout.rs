use std::{
    sync::{Arc, Barrier},
    thread,
};

use oxiroute_rtmp::{
    LiveHub, LiveHubError, LiveHubLimits, MAX_FLV_TAG_DATA_SIZE, MediaEvent, MediaEventError,
    MediaEventKind, PlaybackSubscription, StreamKey, StreamMetadata, VideoCodec,
    VideoCodecIdentifier,
};

#[test]
fn classifies_immutable_media_events() {
    let metadata = metadata(0x01);
    let payload = metadata.payload_arc();
    let cloned = metadata.clone();
    assert_eq!(metadata.kind(), MediaEventKind::Metadata);
    assert_eq!(metadata.timestamp_ms(), 0);
    assert!(Arc::ptr_eq(&payload, &cloned.payload_arc()));

    assert_eq!(
        MediaEvent::audio(10, vec![0xaf, 0x00, 0x12])
            .expect("AAC sequence header")
            .kind(),
        MediaEventKind::AacSequenceHeader
    );
    assert_eq!(
        MediaEvent::audio(11, vec![0xaf, 0x01, 0x33])
            .expect("AAC media")
            .kind(),
        MediaEventKind::Audio
    );
    assert_eq!(
        MediaEvent::audio(12, vec![0x2f, 0x44])
            .expect("non-AAC audio")
            .kind(),
        MediaEventKind::Audio
    );
    assert_eq!(avc_header(20).kind(), MediaEventKind::AvcSequenceHeader);
    assert_eq!(keyframe(21, 0x11).kind(), MediaEventKind::VideoKeyframe);
    assert_eq!(interframe(22, 0x22).kind(), MediaEventKind::VideoInterframe);
    assert_eq!(disposable(23, 0x33).kind(), MediaEventKind::VideoDisposable);
    assert_eq!(
        MediaEvent::video(24, vec![0x47, 0x01, 0x00, 0x00, 0x00, 0x44])
            .expect("generated keyframe")
            .kind(),
        MediaEventKind::VideoKeyframe
    );

    assert_eq!(
        MediaEvent::audio(0, vec![0xaf, 0x02]),
        Err(MediaEventError::MalformedAudio)
    );
    assert_eq!(
        MediaEvent::video(0, vec![0x57]),
        Err(MediaEventError::MalformedVideo)
    );
    assert_eq!(
        MediaEvent::video(0, vec![0x12, 0x44]),
        Err(MediaEventError::UnsupportedVideoCodec(
            VideoCodecIdentifier::Flv(2)
        ))
    );
    assert_eq!(
        MediaEvent::audio(0, vec![0x2f; MAX_FLV_TAG_DATA_SIZE + 1]),
        Err(MediaEventError::PayloadTooLarge {
            size: MAX_FLV_TAG_DATA_SIZE + 1,
            maximum: MAX_FLV_TAG_DATA_SIZE,
        })
    );
}

#[test]
fn classifies_enhanced_avc_hevc_and_av1_video() {
    for (four_cc, codec, sequence_kind) in [
        (*b"avc1", VideoCodec::Avc, MediaEventKind::AvcSequenceHeader),
        (
            *b"hvc1",
            VideoCodec::Hevc,
            MediaEventKind::HevcSequenceHeader,
        ),
        (*b"av01", VideoCodec::Av1, MediaEventKind::Av1SequenceHeader),
    ] {
        let sequence = enhanced_video(1, 1, 0, four_cc, 0x01).expect("sequence header");
        assert_eq!(sequence.kind(), sequence_kind);
        assert_eq!(sequence.video_codec(), Some(codec));

        let keyframe = enhanced_video(2, 1, 1, four_cc, 0x02).expect("coded keyframe");
        assert_eq!(keyframe.kind(), MediaEventKind::VideoKeyframe);
        assert_eq!(keyframe.video_codec(), Some(codec));

        let interframe = enhanced_video(3, 2, 3, four_cc, 0x03).expect("coded interframe");
        assert_eq!(interframe.kind(), MediaEventKind::VideoInterframe);
        assert_eq!(interframe.video_codec(), Some(codec));
    }

    assert_eq!(
        enhanced_video(4, 1, 1, *b"vp09", 0x04),
        Err(MediaEventError::UnsupportedVideoCodec(
            VideoCodecIdentifier::FourCc(*b"vp09")
        ))
    );
    assert_eq!(
        enhanced_video(5, 2, 0, *b"hvc1", 0x05),
        Err(MediaEventError::MalformedVideo)
    );
}

#[test]
fn admits_only_one_concurrent_publisher_for_an_exact_key() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let barrier = Arc::new(Barrier::new(3));
    let mut attempts = Vec::new();

    for _ in 0..2 {
        let hub = hub.clone();
        let barrier = Arc::clone(&barrier);
        attempts.push(thread::spawn(move || {
            let result = hub.attach_publisher(key("camera"));
            barrier.wait();
            result
        }));
    }

    barrier.wait();
    let results: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.join().expect("publisher thread"))
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert!(
        results
            .iter()
            .any(|result| matches!(result, Err(LiveHubError::PublisherAlreadyAttached { .. })))
    );
    let lease = results
        .into_iter()
        .find_map(Result::ok)
        .expect("winning publisher");
    assert_eq!(lease.key(), &key("camera"));
    assert_eq!(hub.stats().publishers, 1);
}

#[test]
fn hub_keys_are_exact_and_do_not_strip_queries_implicitly() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let first = hub
        .attach_publisher(key("camera?token=first"))
        .expect("first exact key");
    let second = hub
        .attach_publisher(key("camera?token=second"))
        .expect("second exact key");

    assert_ne!(first.key(), second.key());
    assert_eq!(hub.stats().publishers, 2);
}

#[test]
fn audio_only_viewers_bootstrap_on_audio_and_late_join() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let waiting = hub.subscribe(key("radio")).expect("waiting viewer");
    let publisher = hub.attach_publisher(key("radio")).expect("publisher");
    publisher
        .publish(audio_metadata(0x01))
        .expect("audio metadata");
    publisher.publish(aac_header(1)).expect("AAC header");
    publisher.publish(audio(2, 0x02)).expect("first audio");

    assert_eq!(
        drain_kinds(&waiting),
        [
            MediaEventKind::Metadata,
            MediaEventKind::AacSequenceHeader,
            MediaEventKind::Audio,
        ]
    );
    assert!(!waiting.is_waiting_for_keyframe());

    let late = hub.subscribe(key("radio")).expect("late viewer");
    assert!(late.try_next().is_none());
    publisher.publish(audio(3, 0x03)).expect("next audio");
    assert_eq!(
        drain_kinds(&late),
        [
            MediaEventKind::Metadata,
            MediaEventKind::AacSequenceHeader,
            MediaEventKind::Audio,
        ]
    );
}

#[test]
fn audio_only_queue_overflow_rebootstraps_without_waiting_for_video() {
    let hub = LiveHub::new(LiveHubLimits {
        max_queue_messages_per_subscriber: 3,
        ..LiveHubLimits::default()
    });
    let viewer = hub.subscribe(key("radio")).expect("viewer");
    let publisher = hub.attach_publisher(key("radio")).expect("publisher");
    publisher
        .publish(audio_metadata(0x01))
        .expect("audio metadata");
    publisher.publish(aac_header(1)).expect("AAC header");
    publisher.publish(audio(2, 0x02)).expect("first audio");

    let report = publisher.publish(audio(3, 0x03)).expect("overflow audio");
    assert_eq!(report.viewers_resynchronized, 1);
    assert!(!viewer.is_waiting_for_keyframe());
    assert_eq!(viewer.queued_messages(), 3);
    assert_eq!(
        viewer.try_next().expect("metadata").kind(),
        MediaEventKind::Metadata
    );
    assert_eq!(
        viewer.try_next().expect("AAC header").kind(),
        MediaEventKind::AacSequenceHeader
    );
    let latest = viewer.try_next().expect("latest audio");
    assert_eq!(latest.kind(), MediaEventKind::Audio);
    assert_eq!(latest.timestamp_ms(), 3);
}

#[test]
fn metadata_declared_mixed_stream_waits_for_a_video_keyframe() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let publisher = hub.attach_publisher(key("camera")).expect("publisher");
    publisher.publish(metadata(0x01)).expect("mixed metadata");
    publisher.publish(aac_header(1)).expect("AAC header");
    let viewer = hub.subscribe(key("camera")).expect("late viewer");

    publisher.publish(audio(2, 0x02)).expect("gated audio");
    assert!(viewer.try_next().is_none());
    publisher
        .publish(keyframe(3, 0x03))
        .expect("bootstrap keyframe");
    assert_eq!(
        drain_kinds(&viewer),
        [
            MediaEventKind::Metadata,
            MediaEventKind::AacSequenceHeader,
            MediaEventKind::VideoKeyframe,
        ]
    );
}

#[test]
fn authoritative_audio_only_metadata_wakes_keyframe_waiters() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let publisher = hub.attach_publisher(key("camera")).expect("publisher");
    publisher.publish(metadata(0x01)).expect("mixed metadata");
    publisher.publish(aac_header(1)).expect("AAC header");
    let viewer = hub.subscribe(key("camera")).expect("waiting viewer");
    publisher.publish(audio(2, 0x02)).expect("gated audio");
    assert!(viewer.try_next().is_none());

    publisher
        .publish(audio_metadata(0x03))
        .expect("authoritative audio-only metadata");
    publisher.publish(audio(4, 0x04)).expect("audio bootstrap");
    assert_eq!(
        drain_kinds(&viewer),
        [
            MediaEventKind::Metadata,
            MediaEventKind::AacSequenceHeader,
            MediaEventKind::Audio,
        ]
    );
    assert!(!viewer.is_waiting_for_keyframe());
}

#[test]
fn codec_switch_never_pairs_a_keyframe_with_a_stale_sequence_header() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let publisher = hub.attach_publisher(key("camera")).expect("publisher");
    publisher.publish(avc_header(1)).expect("AVC header");
    let viewer = hub.subscribe(key("camera")).expect("viewer");

    publisher
        .publish(enhanced_video(2, 1, 1, *b"hvc1", 0x02).expect("HEVC keyframe"))
        .expect("HEVC codec switch");
    assert_eq!(drain_kinds(&viewer), [MediaEventKind::VideoKeyframe]);

    publisher
        .publish(enhanced_video(3, 1, 0, *b"hvc1", 0x03).expect("HEVC header"))
        .expect("cache HEVC header");
    publisher
        .publish(enhanced_video(4, 1, 1, *b"hvc1", 0x04).expect("HEVC keyframe"))
        .expect("HEVC bootstrap");
    assert_eq!(
        drain_kinds(&viewer),
        [
            MediaEventKind::HevcSequenceHeader,
            MediaEventKind::VideoKeyframe,
        ]
    );

    publisher
        .publish(enhanced_video(5, 1, 1, *b"av01", 0x05).expect("AV1 keyframe"))
        .expect("AV1 codec switch");
    assert_eq!(drain_kinds(&viewer), [MediaEventKind::VideoKeyframe]);
}

#[test]
fn audio_started_stream_keeps_audio_flowing_while_late_video_waits_for_a_keyframe() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let publisher = hub.attach_publisher(key("event")).expect("publisher");
    publisher
        .publish(audio_metadata(0x01))
        .expect("audio metadata");
    publisher.publish(aac_header(1)).expect("AAC header");
    let viewer = hub.subscribe(key("event")).expect("viewer");
    publisher.publish(audio(2, 0x02)).expect("audio bootstrap");
    assert_eq!(
        drain_kinds(&viewer),
        [
            MediaEventKind::Metadata,
            MediaEventKind::AacSequenceHeader,
            MediaEventKind::Audio,
        ]
    );

    publisher.publish(avc_header(3)).expect("late AVC header");
    publisher
        .publish(interframe(4, 0x04))
        .expect("gated interframe");
    publisher.publish(audio(5, 0x05)).expect("ongoing audio");
    assert_eq!(drain_kinds(&viewer), [MediaEventKind::Audio]);

    publisher
        .publish(keyframe(6, 0x06))
        .expect("first video keyframe");
    assert_eq!(
        drain_kinds(&viewer),
        [
            MediaEventKind::AvcSequenceHeader,
            MediaEventKind::VideoKeyframe,
        ]
    );
}

#[test]
fn enhanced_video_late_join_bootstraps_with_the_matching_sequence_header() {
    for (name, four_cc, expected_header) in [
        ("avc", *b"avc1", MediaEventKind::AvcSequenceHeader),
        ("hevc", *b"hvc1", MediaEventKind::HevcSequenceHeader),
        ("av1", *b"av01", MediaEventKind::Av1SequenceHeader),
    ] {
        let hub = LiveHub::new(LiveHubLimits::default());
        let publisher = hub.attach_publisher(key(name)).expect("publisher");
        publisher
            .publish(enhanced_video(1, 1, 0, four_cc, 0x01).expect("sequence header"))
            .expect("cache sequence header");
        let viewer = hub.subscribe(key(name)).expect("late viewer");
        publisher
            .publish(enhanced_video(2, 2, 1, four_cc, 0x02).expect("interframe"))
            .expect("gated interframe");
        assert!(viewer.try_next().is_none());
        publisher
            .publish(enhanced_video(3, 1, 1, four_cc, 0x03).expect("keyframe"))
            .expect("bootstrap keyframe");
        assert_eq!(
            drain_kinds(&viewer),
            [expected_header, MediaEventKind::VideoKeyframe]
        );
    }
}

#[test]
fn viewer_can_wait_before_the_publisher_and_starts_on_a_keyframe() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let viewer = hub.subscribe(key("camera")).expect("idle viewer");
    assert_eq!(
        hub.stats(),
        oxiroute_rtmp::LiveHubStats {
            streams: 1,
            publishers: 0,
            subscribers: 1,
            fanout_bytes: 0,
        }
    );
    assert!(viewer.try_next().is_none());

    let publisher = hub.attach_publisher(key("camera")).expect("publisher");
    publisher.publish(metadata(0x01)).expect("cache metadata");
    publisher
        .publish(interframe(10, 0x02))
        .expect("gated interframe");
    assert!(viewer.try_next().is_none());

    publisher
        .publish(keyframe(20, 0x03))
        .expect("first keyframe");
    assert_eq!(
        drain_kinds(&viewer),
        [MediaEventKind::Metadata, MediaEventKind::VideoKeyframe]
    );
}

#[test]
fn late_viewer_gets_cached_headers_in_order_at_the_next_keyframe() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let publisher = hub.attach_publisher(key("camera")).expect("publisher");
    publisher.publish(metadata(0x01)).expect("metadata");
    publisher.publish(aac_header(10)).expect("AAC header");
    publisher.publish(avc_header(11)).expect("AVC header");
    publisher
        .publish(keyframe(12, 0x12))
        .expect("prior keyframe");

    let viewer = hub.subscribe(key("camera")).expect("late viewer");
    publisher.publish(audio(13, 0x13)).expect("gated audio");
    publisher
        .publish(interframe(14, 0x14))
        .expect("gated interframe");
    assert_eq!(viewer.queued_messages(), 0);

    publisher
        .publish(keyframe(15, 0x15))
        .expect("future keyframe");
    let expected_bytes = metadata(0x01).payload_len()
        + aac_header(10).payload_len()
        + avc_header(11).payload_len()
        + keyframe(15, 0x15).payload_len();
    assert_eq!(viewer.queued_bytes(), expected_bytes);
    assert_eq!(hub.stats().fanout_bytes, expected_bytes);
    assert_eq!(
        drain_kinds(&viewer),
        [
            MediaEventKind::Metadata,
            MediaEventKind::AacSequenceHeader,
            MediaEventKind::AvcSequenceHeader,
            MediaEventKind::VideoKeyframe,
        ]
    );
    assert_eq!(hub.stats().fanout_bytes, 0);
}

#[test]
fn saturated_viewer_resynchronizes_without_harming_a_healthy_viewer() {
    let hub = LiveHub::new(LiveHubLimits {
        max_queue_messages_per_subscriber: 4,
        ..LiveHubLimits::default()
    });
    let slow = hub.subscribe(key("camera")).expect("slow viewer");
    let healthy = hub.subscribe(key("camera")).expect("healthy viewer");
    let publisher = hub.attach_publisher(key("camera")).expect("publisher");
    publisher.publish(metadata(0x01)).expect("metadata");
    publisher.publish(aac_header(1)).expect("AAC header");
    publisher.publish(avc_header(2)).expect("AVC header");
    publisher
        .publish(keyframe(3, 0x03))
        .expect("initial keyframe");
    assert_eq!(slow.queued_messages(), 4);
    assert_eq!(drain_kinds(&healthy).len(), 4);

    let disposable_report = publisher
        .publish(disposable(4, 0x04))
        .expect("disposable frame");
    assert_eq!(disposable_report.dropped_events, 1);
    assert!(!slow.is_waiting_for_keyframe());
    assert_eq!(drain_kinds(&healthy), [MediaEventKind::VideoDisposable]);

    let resync_report = publisher.publish(interframe(5, 0x05)).expect("interframe");
    assert_eq!(resync_report.viewers_resynchronized, 1);
    assert_eq!(slow.queued_messages(), 0);
    assert!(slow.is_waiting_for_keyframe());
    publisher.publish(audio(6, 0x06)).expect("audio");
    publisher
        .publish(keyframe(7, 0x07))
        .expect("resync keyframe");

    assert_eq!(
        drain_kinds(&slow),
        [
            MediaEventKind::Metadata,
            MediaEventKind::AacSequenceHeader,
            MediaEventKind::AvcSequenceHeader,
            MediaEventKind::VideoKeyframe,
        ]
    );
    assert_eq!(
        drain_kinds(&healthy),
        [
            MediaEventKind::VideoInterframe,
            MediaEventKind::Audio,
            MediaEventKind::VideoKeyframe,
        ]
    );
}

#[test]
fn publisher_detach_and_restart_clear_queues_and_cached_headers() {
    let hub = LiveHub::new(LiveHubLimits::default());
    let viewer = hub.subscribe(key("camera")).expect("viewer");
    let first = hub
        .attach_publisher(key("camera"))
        .expect("first publisher");
    let first_incarnation = first.incarnation();
    first.publish(metadata(0x01)).expect("metadata");
    first.publish(avc_header(1)).expect("AVC header");
    first.publish(keyframe(2, 0x02)).expect("first keyframe");
    assert!(viewer.queued_messages() > 0);
    drop(first);

    assert_eq!(viewer.queued_messages(), 0);
    assert!(viewer.is_waiting_for_keyframe());
    assert_eq!(hub.stats().fanout_bytes, 0);
    assert_eq!(hub.stats().publishers, 0);

    let second = hub
        .attach_publisher(key("camera"))
        .expect("replacement publisher");
    assert_ne!(second.incarnation(), first_incarnation);
    second
        .publish(interframe(3, 0x03))
        .expect("gated interframe");
    second
        .publish(keyframe(4, 0x04))
        .expect("replacement keyframe");
    assert_eq!(drain_kinds(&viewer), [MediaEventKind::VideoKeyframe]);

    drop(second);
    drop(viewer);
    assert_eq!(hub.stats().streams, 0);
}

#[test]
fn enforces_stream_and_subscriber_caps_without_leaking_entries() {
    let stream_limited = LiveHub::new(LiveHubLimits {
        max_streams: 1,
        ..LiveHubLimits::default()
    });
    let viewer = stream_limited.subscribe(key("one")).expect("first stream");
    assert!(matches!(
        stream_limited.attach_publisher(key("two")),
        Err(LiveHubError::StreamLimitReached { maximum: 1 })
    ));
    assert_eq!(stream_limited.stats().streams, 1);
    drop(viewer);
    assert_eq!(stream_limited.stats().streams, 0);

    let service_limited = LiveHub::new(LiveHubLimits {
        max_subscribers: 1,
        ..LiveHubLimits::default()
    });
    let viewer = service_limited.subscribe(key("one")).expect("first viewer");
    assert!(matches!(
        service_limited.subscribe(key("two")),
        Err(LiveHubError::SubscriberLimitReached { maximum: 1 })
    ));
    assert_eq!(service_limited.stats().streams, 1);
    drop(viewer);

    let stream_subscriber_limited = LiveHub::new(LiveHubLimits {
        max_subscribers_per_stream: 1,
        ..LiveHubLimits::default()
    });
    let _viewer = stream_subscriber_limited
        .subscribe(key("one"))
        .expect("first viewer");
    assert!(matches!(
        stream_subscriber_limited.subscribe(key("one")),
        Err(LiveHubError::StreamSubscriberLimitReached { maximum: 1, .. })
    ));
    assert_eq!(stream_subscriber_limited.stats().subscribers, 1);
}

#[test]
fn enforces_message_byte_global_and_cache_caps() {
    let message_limited = LiveHub::new(LiveHubLimits {
        max_queue_messages_per_subscriber: 3,
        ..LiveHubLimits::default()
    });
    let viewer = message_limited.subscribe(key("camera")).expect("viewer");
    let publisher = message_limited
        .attach_publisher(key("camera"))
        .expect("publisher");
    publisher.publish(metadata(0x01)).expect("metadata");
    publisher.publish(aac_header(1)).expect("AAC header");
    publisher.publish(avc_header(2)).expect("AVC header");
    publisher
        .publish(keyframe(3, 0x03))
        .expect("bounded keyframe");
    assert_eq!(viewer.queued_messages(), 0);
    assert_eq!(message_limited.stats().fanout_bytes, 0);

    let byte_limited = LiveHub::new(LiveHubLimits {
        max_queue_bytes_per_subscriber: 5,
        ..LiveHubLimits::default()
    });
    let viewer = byte_limited.subscribe(key("camera")).expect("viewer");
    let publisher = byte_limited
        .attach_publisher(key("camera"))
        .expect("publisher");
    publisher
        .publish(keyframe(1, 0x01))
        .expect("oversized keyframe for queue");
    assert_eq!(viewer.queued_bytes(), 0);

    let global_limited = LiveHub::new(LiveHubLimits {
        max_fanout_bytes: 6,
        ..LiveHubLimits::default()
    });
    let first = global_limited
        .subscribe(key("camera"))
        .expect("first viewer");
    let second = global_limited
        .subscribe(key("camera"))
        .expect("second viewer");
    let publisher = global_limited
        .attach_publisher(key("camera"))
        .expect("publisher");
    publisher
        .publish(keyframe(1, 0x01))
        .expect("globally bounded keyframe");
    assert_eq!(first.queued_bytes(), 6);
    assert_eq!(second.queued_bytes(), 0);
    assert_eq!(global_limited.stats().fanout_bytes, 6);
    drop(first);
    assert_eq!(global_limited.stats().fanout_bytes, 0);
    assert_eq!(global_limited.stats().subscribers, 1);

    let oversized_metadata = metadata(0x01);
    let metadata_size = oversized_metadata.payload_len();
    let cache_limited = LiveHub::new(LiveHubLimits {
        max_cached_metadata_bytes: metadata_size - 1,
        max_cached_codec_header_bytes: 5,
        ..LiveHubLimits::default()
    });
    let publisher = cache_limited
        .attach_publisher(key("camera"))
        .expect("publisher");
    assert!(matches!(
        publisher.publish(oversized_metadata),
        Err(LiveHubError::CachedEventTooLarge {
            kind: MediaEventKind::Metadata,
            size,
            maximum,
        }) if size == metadata_size && maximum == metadata_size - 1
    ));
    assert!(matches!(
        publisher.publish(avc_header(1)),
        Err(LiveHubError::CachedEventTooLarge {
            kind: MediaEventKind::AvcSequenceHeader,
            size: 6,
            maximum: 5,
        })
    ));
}

fn key(name: &str) -> StreamKey {
    StreamKey::new("edge", "live", name)
}

fn metadata(marker: u8) -> MediaEvent {
    let mut metadata = StreamMetadata::new();
    metadata.video_codec_id = Some(7);
    metadata.audio_codec_id = Some(10);
    metadata.encoder = Some(format!("test-{marker}"));
    MediaEvent::metadata(metadata).expect("metadata event")
}

fn audio_metadata(marker: u8) -> MediaEvent {
    let mut metadata = StreamMetadata::new();
    metadata.audio_codec_id = Some(10);
    metadata.encoder = Some(format!("audio-test-{marker}"));
    MediaEvent::metadata(metadata).expect("audio metadata event")
}

fn aac_header(timestamp_ms: u32) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, vec![0xaf, 0x00, 0x12]).expect("AAC header event")
}

fn audio(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, vec![0xaf, 0x01, marker]).expect("audio event")
}

fn avc_header(timestamp_ms: u32) -> MediaEvent {
    MediaEvent::video(timestamp_ms, vec![0x17, 0x00, 0x00, 0x00, 0x00, 0x01])
        .expect("AVC header event")
}

fn keyframe(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, vec![0x17, 0x01, 0x00, 0x00, 0x00, marker])
        .expect("keyframe event")
}

fn interframe(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, vec![0x27, 0x01, 0x00, 0x00, 0x00, marker])
        .expect("interframe event")
}

fn disposable(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::video(timestamp_ms, vec![0x37, 0x01, 0x00, 0x00, 0x00, marker])
        .expect("disposable event")
}

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
