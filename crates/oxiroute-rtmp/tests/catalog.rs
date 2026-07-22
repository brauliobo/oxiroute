use oxiroute_rtmp::{
    CatalogError, MediaSnapshot, RecorderCompletion, RecorderDefinition, RecorderPhase,
    RecordingAction, RtmpCapabilities, RtmpRegistry, SessionId, StreamKey, TrackSnapshot,
};

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
            payload_bytes_received: 1_024,
            last_rtmp_timestamp_ms: Some(u32::MAX),
            last_observed_at_unix_ms: Some(200),
        },
        video: TrackSnapshot::default(),
        fanout_payload_bytes_queued: 2_048,
    };

    assert!(registry
        .update_media_sample(stream_id, publisher, 2, sample, 200)
        .expect("new sample"));
    assert!(!registry
        .update_media_sample(stream_id, publisher, 1, MediaSnapshot::default(), 300)
        .expect("stale sample"));

    let visible = registry.snapshot();
    assert_eq!(visible.streams[0].media, sample);
    assert_eq!(visible.streams[0].media.audio.flv_codec_id, Some(42));
    assert_eq!(
        visible.streams[0].media.audio.last_rtmp_timestamp_ms,
        Some(u32::MAX)
    );
}

#[test]
fn tracks_idempotent_manual_recording_transitions() {
    let registry = RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    });
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            publisher,
            vec![RecorderDefinition::manual(Some("archive".into()))],
            100,
        )
        .expect("publisher attachment");
    let recorder_id = registry.snapshot().streams[0].recorders[0].id;

    let starting = registry
        .request_recording(stream_id, recorder_id, RecordingAction::Start, 200)
        .expect("start request");
    let repeated = registry
        .request_recording(stream_id, recorder_id, RecordingAction::Start, 201)
        .expect("idempotent start");
    assert_eq!(starting.phase, repeated.phase);
    let RecorderPhase::Starting { operation_id } = starting.phase else {
        panic!("expected starting recorder");
    };

    registry
        .complete_recording(
            stream_id,
            publisher,
            recorder_id,
            operation_id,
            RecorderCompletion::Started,
            300,
        )
        .expect("recording started");
    assert!(matches!(
        registry.snapshot().streams[0].recorders[0].phase,
        RecorderPhase::Recording { .. }
    ));

    let stopping = registry
        .request_recording(stream_id, recorder_id, RecordingAction::Stop, 400)
        .expect("stop request");
    let RecorderPhase::Stopping { operation_id } = stopping.phase else {
        panic!("expected stopping recorder");
    };
    registry
        .complete_recording(
            stream_id,
            publisher,
            recorder_id,
            operation_id,
            RecorderCompletion::Stopped,
            500,
        )
        .expect("recording stopped");
    assert_eq!(
        registry.snapshot().streams[0].recorders[0].phase,
        RecorderPhase::Idle
    );
}

#[test]
fn refuses_recording_when_the_runtime_capability_is_absent() {
    let registry = RtmpRegistry::new(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    });
    let publisher = SessionId::new();
    let stream_id = registry
        .attach_publisher(
            StreamKey::new("edge", "live", "camera"),
            publisher,
            vec![RecorderDefinition::manual(None)],
            100,
        )
        .expect("model attachment");
    let recorder_id = registry.snapshot().streams[0].recorders[0].id;

    assert_eq!(
        registry.request_recording(stream_id, recorder_id, RecordingAction::Start, 200),
        Err(CatalogError::RecordingUnavailable)
    );
}
