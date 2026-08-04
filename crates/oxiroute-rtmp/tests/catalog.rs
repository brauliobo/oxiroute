use std::{sync::Arc, thread};

use oxiroute_rtmp::{
    CatalogError, MAX_RTMP_APPLICATION_BYTES, MAX_RTMP_QUERY_BYTES, MAX_RTMP_STREAM_NAME_BYTES,
    MediaSnapshot, RecorderDefinition, RtmpCapabilities, RtmpRegistry, RtmpStreamPath,
    RtmpStreamPathError, SessionId, StreamKey, TrackSnapshot,
};

#[test]
fn parses_explicit_rtmp_application_and_stream_components() {
    let path = RtmpStreamPath::parse("live", "camera?token=secret").expect("valid RTMP path");
    assert_eq!(path.application(), "live");
    assert_eq!(path.stream_name(), "camera");
    assert_eq!(path.query(), Some("token=secret"));
    assert_eq!(
        path.stream_key("broadcast-service"),
        StreamKey::new("broadcast-service", "live", "camera")
    );

    for (application, stream) in [
        ("", "camera"),
        ("live/nested", "camera"),
        ("live?token=secret", "camera"),
        ("live", ""),
        ("live", "?token=secret"),
        ("live", "nested/camera"),
        ("live", "camera?"),
        ("live", "camera#fragment"),
    ] {
        assert!(
            RtmpStreamPath::parse(application, stream).is_err(),
            "accepted {application:?}/{stream:?}"
        );
    }
}

#[test]
fn redacts_stream_query_arguments_from_debug_output() {
    let path = RtmpStreamPath::parse("live", "camera?token=secret").expect("valid RTMP path");
    let debug = format!("{path:?}");

    assert!(!debug.contains("token=secret"));
    assert!(debug.contains("<redacted>"));
}

#[test]
fn bounds_rtmp_identity_components_before_retaining_them() {
    let application = "a".repeat(MAX_RTMP_APPLICATION_BYTES);
    let stream = "s".repeat(MAX_RTMP_STREAM_NAME_BYTES);
    let query = "q".repeat(MAX_RTMP_QUERY_BYTES);
    let path = RtmpStreamPath::parse(&application, &format!("{stream}?{query}"))
        .expect("identity at every byte limit");
    assert_eq!(path.application().len(), MAX_RTMP_APPLICATION_BYTES);
    assert_eq!(path.stream_name().len(), MAX_RTMP_STREAM_NAME_BYTES);
    assert_eq!(path.query().map(str::len), Some(MAX_RTMP_QUERY_BYTES));

    assert_eq!(
        RtmpStreamPath::parse(&"a".repeat(MAX_RTMP_APPLICATION_BYTES + 1), "stream"),
        Err(RtmpStreamPathError::ApplicationTooLong {
            size: MAX_RTMP_APPLICATION_BYTES + 1,
            maximum: MAX_RTMP_APPLICATION_BYTES,
        })
    );
    assert_eq!(
        RtmpStreamPath::parse("live", &"s".repeat(MAX_RTMP_STREAM_NAME_BYTES + 1)),
        Err(RtmpStreamPathError::StreamNameTooLong {
            size: MAX_RTMP_STREAM_NAME_BYTES + 1,
            maximum: MAX_RTMP_STREAM_NAME_BYTES,
        })
    );
    assert_eq!(
        RtmpStreamPath::parse(
            "live",
            &format!("camera?{}", "q".repeat(MAX_RTMP_QUERY_BYTES + 1)),
        ),
        Err(RtmpStreamPathError::QueryTooLong {
            size: MAX_RTMP_QUERY_BYTES + 1,
            maximum: MAX_RTMP_QUERY_BYTES,
        })
    );
}

#[test]
fn snapshots_active_streams_without_mutating_prior_revisions() {
    let registry = RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    });
    let key = StreamKey::new("edge", "live", "camera");
    let subscriber = SessionId::new();
    let stream_id = registry
        .attach_subscriber(key.clone(), subscriber, 100)
        .expect("subscriber attachment");
    let subscriber_only = registry.snapshot();

    assert_eq!(subscriber_only.revision, 1);
    assert_eq!(subscriber_only.streams.len(), 1);
    assert_eq!(subscriber_only.streams[0].subscriber_count, 1);
    assert!(subscriber_only.streams[0].publisher.is_none());

    let publisher = SessionId::new();
    assert_eq!(
        registry
            .attach_publisher(key, publisher, Vec::new(), 200)
            .expect("publisher attachment"),
        stream_id
    );
    let catalog_after_publish = registry.snapshot();
    assert_eq!(
        catalog_after_publish.streams[0]
            .publisher
            .as_ref()
            .unwrap()
            .session_id,
        publisher
    );
    assert!(subscriber_only.streams[0].publisher.is_none());

    assert!(matches!(
        registry.attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            SessionId::new(),
            Vec::new(),
            300,
        ),
        Err(CatalogError::PublisherAlreadyAttached { .. })
    ));

    registry
        .detach_publisher(stream_id, publisher, 400)
        .expect("publisher detach");
    assert_eq!(registry.snapshot().streams[0].id, stream_id);
    registry
        .detach_subscriber(stream_id, subscriber, 500)
        .expect("subscriber detach");
    assert!(registry.snapshot().streams.is_empty());

    let replacement = registry
        .attach_subscriber(
            StreamKey::new("edge", "live", "camera"),
            SessionId::new(),
            600,
        )
        .expect("replacement stream");
    assert_ne!(replacement, stream_id);
}

#[test]
fn publishes_absolute_media_samples_and_ignores_stale_sequences() {
    let registry = RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    });
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            publisher,
            Vec::new(),
            100,
        )
        .expect("publisher attachment");
    let sample = MediaSnapshot {
        audio: TrackSnapshot {
            flv_codec_id: Some(42),
            video_codec: None,
            payload_bytes_received: 1_024,
            last_rtmp_timestamp_ms: Some(u32::MAX),
            last_observed_at_unix_ms: Some(200),
        },
        video: TrackSnapshot::default(),
        fanout_payload_bytes_queued: 2_048,
    };

    assert!(
        registry
            .update_media_sample(stream_id, publisher, 2, sample, 200)
            .expect("new sample")
    );
    assert!(
        !registry
            .update_media_sample(stream_id, publisher, 1, MediaSnapshot::default(), 300)
            .expect("stale sample")
    );

    let visible = registry.snapshot();
    assert_eq!(visible.streams[0].media, sample);
    assert_eq!(visible.streams[0].media.audio.flv_codec_id, Some(42));
    assert_eq!(
        visible.streams[0].media.audio.last_rtmp_timestamp_ms,
        Some(u32::MAX)
    );
}

#[test]
fn media_updates_are_constant_work_until_a_snapshot_is_requested() {
    const STREAMS: usize = 64;
    const UPDATES: u64 = 256;

    let registry = RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    });
    let mut publishers = Vec::with_capacity(STREAMS);
    for index in 0..STREAMS {
        let publisher = SessionId::new();
        let stream_id = registry
            .attach_publisher(
                StreamKey::new("edge", "live", format!("camera-{index}")),
                publisher,
                Vec::new(),
                index as u64,
            )
            .expect("publisher attachment");
        publishers.push((stream_id, publisher));
    }
    let before = registry.work_stats();
    let (stream_id, publisher) = publishers[0];

    for sequence in 1..=UPDATES {
        registry
            .update_media_sample(
                stream_id,
                publisher,
                sequence,
                MediaSnapshot {
                    fanout_payload_bytes_queued: sequence,
                    ..MediaSnapshot::default()
                },
                10_000 + sequence,
            )
            .expect("media update");
    }

    let after_updates = registry.work_stats();
    assert_eq!(after_updates.snapshot_rebuilds, before.snapshot_rebuilds);
    assert_eq!(
        after_updates.snapshot_streams_visited,
        before.snapshot_streams_visited
    );
    assert_eq!(after_updates.media_updates, before.media_updates + UPDATES);

    let snapshot = registry.snapshot();
    let after_snapshot = registry.work_stats();
    assert_eq!(
        after_snapshot.snapshot_rebuilds,
        before.snapshot_rebuilds + 1
    );
    assert_eq!(
        after_snapshot.snapshot_streams_visited,
        before.snapshot_streams_visited + STREAMS as u64
    );
    assert_eq!(
        snapshot
            .streams
            .iter()
            .find(|stream| stream.id == stream_id)
            .expect("updated stream")
            .media
            .fanout_payload_bytes_queued,
        UPDATES
    );
    assert!(Arc::ptr_eq(&snapshot, &registry.snapshot()));
}

#[test]
fn catalog_time_never_regresses_on_lazy_updates_or_raii_drop() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let publisher = SessionId::new();
    let mut registration = registry
        .register_publisher(
            StreamKey::new("edge", "live", "camera"),
            publisher,
            Vec::new(),
            1_000,
        )
        .expect("publisher registration");
    registry
        .update_media_sample(
            registration.stream_id(),
            publisher,
            1,
            MediaSnapshot::default(),
            2_000,
        )
        .expect("newer media update");
    let media_revision = registry.snapshot().revision;

    registration.observe_at(500);
    drop(registration);
    let detached = registry.snapshot();
    assert!(detached.streams.is_empty());
    assert!(detached.revision > media_revision);
    assert_eq!(detached.as_of_unix_ms, 2_000);

    let registry_for_thread = Arc::clone(&registry);
    thread::spawn(move || {
        let _subscriber = registry_for_thread
            .register_subscriber(
                StreamKey::new("edge", "live", "waiting"),
                SessionId::new(),
                100,
            )
            .expect("subscriber registration");
    })
    .join()
    .expect("subscriber thread");
    assert_eq!(registry.snapshot().as_of_unix_ms, 2_000);
}

#[test]
fn unmanaged_catalog_publishers_cannot_fabricate_recorders() {
    let registry = RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    });
    assert_eq!(
        registry.attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            SessionId::new(),
            vec![RecorderDefinition::manual(Some("archive".into()))],
            100,
        ),
        Err(CatalogError::RecordingUnavailable)
    );
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn unmanaged_recorders_are_rejected_regardless_of_advertised_capability() {
    let registry = RtmpRegistry::new(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    });
    assert_eq!(
        registry.attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            SessionId::new(),
            vec![RecorderDefinition::manual(None)],
            100,
        ),
        Err(CatalogError::RecordingUnavailable)
    );
    assert_eq!(
        CatalogError::RecordingUnavailable.to_string(),
        "RTMP recording is unavailable in the active runtime"
    );
    assert!(registry.snapshot().streams.is_empty());
}
