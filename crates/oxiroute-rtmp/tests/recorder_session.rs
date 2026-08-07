use std::{
    collections::VecDeque,
    fs::OpenOptions,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use oxiroute_rtmp::{
    CatalogError, LiveHub, LiveHubLimits, MediaSnapshot, RTMP_STALE_PUBLISHER_THRESHOLD_MS,
    RecorderErrorCode, RecorderPhase, RecorderWorkerConfig, RecordingPathPolicy, RecordingStore,
    RecordingStoreLimits, RtmpApplication, RtmpCapabilities, RtmpRecorderPolicy, RtmpRecorderStart,
    RtmpRegistry, RtmpServiceRuntime, RtmpSession, RtmpSessionPolicy, SessionId, StreamKey,
};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType, StreamMetadata,
    },
    time::RtmpTimestamp,
};
use rustix::fs::{FlockOperation, flock};
use tempfile::{TempDir, tempdir};

#[test]
fn continuous_recorder_starts_records_and_disconnects_without_leaking_catalog_state() {
    let fixture = Fixture::new(RtmpRecorderStart::Continuous, limits(), worker_config());
    let (mut server, mut client) = fixture.publisher("camera?token=secret");

    publish_audio(&mut client, &mut server, 10, 0x11, 1_721_657_969_100);
    wait_until(Duration::from_secs(2), || {
        fixture.registry.snapshot().streams[0].recorders[0].bytes_written > 0
    });
    let recorder = fixture.registry.snapshot().streams[0].recorders[0].clone();
    assert_eq!(recorder.name.as_deref(), Some("archive"));
    assert!(!recorder.manual);
    assert!(recorder.bytes_written > 0);
    assert_eq!(recorder.segments_started, 1);
    assert_eq!(
        recorder.current_relative_name.as_deref(),
        Some("camera.flv")
    );

    server.close(1_721_657_969_200).expect("publisher close");
    assert!(fixture.registry.snapshot().streams.is_empty());
    wait_for_file(fixture.root.path(), "camera.flv");
    assert!(!fixture.root.path().join("camera?token=secret.flv").exists());
}

#[test]
fn stale_takeover_resets_catalog_media_and_continuous_recorder_state() {
    let fixture = Fixture::new(RtmpRecorderStart::Continuous, limits(), worker_config());
    let (mut first_server, mut first_client) = fixture.publisher("camera");
    let attached_at = 1_721_657_969_000;
    let first_media_at = attached_at + 100;
    publish_audio(
        &mut first_client,
        &mut first_server,
        1,
        0x11,
        first_media_at,
    );
    wait_until(Duration::from_secs(2), || {
        fixture.registry.snapshot().streams[0].recorders[0].bytes_written > 0
    });
    let first = fixture.registry.snapshot().streams[0].clone();
    let first_recorder_id = first.recorders[0].id;
    let takeover_at = first_media_at + RTMP_STALE_PUBLISHER_THRESHOLD_MS;

    let mut second_server = fixture.runtime.session();
    let mut second_client = connect(&mut second_server, "broadcast");
    let request = second_client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("replacement publish request");
    let events = exchange(
        &mut second_client,
        &mut second_server,
        vec![request],
        takeover_at,
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );

    wait_until(Duration::from_secs(2), || {
        let snapshot = fixture.registry.snapshot();
        snapshot.streams.first().is_some_and(|stream| {
            stream
                .publisher
                .is_some_and(|publisher| publisher.session_id == second_server.session_id())
                && stream.recorders.len() == 1
                && stream.recorders[0].id != first_recorder_id
                && matches!(stream.recorders[0].phase, RecorderPhase::Recording { .. })
                && fixture.store.stats().active_recorders == 1
        })
    });
    let replacement = fixture.registry.snapshot().streams[0].clone();
    assert_eq!(replacement.media, MediaSnapshot::default());
    assert_ne!(replacement.recorders[0].id, first_recorder_id);

    drop(first_server);
    assert_eq!(
        fixture.registry.snapshot().streams[0]
            .publisher
            .expect("replacement publisher survives old drop")
            .session_id,
        second_server.session_id()
    );

    publish_audio(
        &mut second_client,
        &mut second_server,
        2,
        0x22,
        takeover_at + 100,
    );
    wait_until(Duration::from_secs(2), || {
        let snapshot = fixture.registry.snapshot();
        snapshot.streams.first().is_some_and(|stream| {
            stream.media.audio.payload_bytes_received == 3 && stream.recorders[0].bytes_written > 0
        })
    });
    second_server
        .close(takeover_at + 200)
        .expect("replacement publisher close");
    wait_until(Duration::from_secs(2), || {
        fixture.store.stats().active_recorders == 0
    });
}

#[test]
fn stale_takeover_retries_continuous_recording_after_a_single_recorder_drains() {
    let fixture = Fixture::new(
        RtmpRecorderStart::Continuous,
        RecordingStoreLimits {
            max_active_recorders: 1,
            ..limits()
        },
        worker_config(),
    );
    let (mut first_server, mut first_client) = fixture.publisher("camera");
    let first_media_at = 1_721_657_969_100;
    publish_audio(
        &mut first_client,
        &mut first_server,
        1,
        0x11,
        first_media_at,
    );
    wait_until(Duration::from_secs(2), || {
        fixture.registry.snapshot().streams[0].recorders[0].bytes_written > 0
    });

    let mut second_server = fixture.runtime.session();
    let mut second_client = connect(&mut second_server, "broadcast");
    let takeover_at = first_media_at + RTMP_STALE_PUBLISHER_THRESHOLD_MS;
    let request = second_client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("replacement publish request");
    let events = exchange(
        &mut second_client,
        &mut second_server,
        vec![request],
        takeover_at,
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
    let replacement = fixture.registry.snapshot().streams[0].clone();
    assert!(matches!(
        replacement.recorders[0].phase,
        RecorderPhase::Starting { .. } | RecorderPhase::Recording { .. }
    ));
    assert!(!matches!(
        replacement.recorders[0].phase,
        RecorderPhase::Failed { .. }
    ));

    publish_audio(
        &mut second_client,
        &mut second_server,
        2,
        0x22,
        takeover_at + 100,
    );
    wait_until(Duration::from_secs(2), || {
        let snapshot = fixture.registry.snapshot();
        snapshot.streams.first().is_some_and(|stream| {
            matches!(stream.recorders[0].phase, RecorderPhase::Recording { .. })
                || fixture.store.stats().active_recorders == 0
        })
    });
    if fixture.store.stats().active_recorders == 0 {
        thread::sleep(Duration::from_millis(300));
    }
    publish_audio(
        &mut second_client,
        &mut second_server,
        3,
        0x33,
        takeover_at + 200,
    );
    wait_until(Duration::from_secs(2), || {
        let snapshot = fixture.registry.snapshot();
        snapshot.streams.first().is_some_and(|stream| {
            matches!(stream.recorders[0].phase, RecorderPhase::Recording { .. })
                && stream.recorders[0].bytes_written > 0
                && stream.media.audio.payload_bytes_received == 6
        })
    });
    second_server
        .close(takeover_at + 300)
        .expect("replacement close");
    wait_until(Duration::from_secs(2), || {
        fixture.store.stats().active_recorders == 0
    });
}

#[test]
fn manual_start_and_stop_control_the_exact_recorder() {
    let fixture = Fixture::new(RtmpRecorderStart::Manual, limits(), worker_config());
    let (mut server, mut client) = fixture.publisher("camera");
    let stream = fixture.registry.snapshot().streams[0].clone();
    let recorder_id = stream.recorders[0].id;
    assert_eq!(stream.recorders[0].phase, RecorderPhase::Idle);

    let starting = fixture
        .registry
        .start_recording(stream.id, recorder_id, 1_721_657_969_100)
        .expect("manual start");
    assert!(matches!(starting.phase, RecorderPhase::Recording { .. }));
    publish_metadata(&mut client, &mut server, 1_721_657_969_150);
    publish_audio(&mut client, &mut server, 10, 0x22, 1_721_657_969_200);
    wait_for_phase(&fixture.registry, |phase| {
        matches!(phase, RecorderPhase::Recording { .. })
    });

    let stopping = fixture
        .registry
        .stop_recording(stream.id, recorder_id, 1_721_657_969_300)
        .expect("manual stop");
    assert!(matches!(stopping.phase, RecorderPhase::Stopping { .. }));
    wait_for_phase(&fixture.registry, |phase| phase == RecorderPhase::Idle);
    wait_for_file(fixture.root.path(), "camera.flv");
    let completed = &fixture.registry.snapshot().streams[0].recorders[0];
    assert_eq!(completed.events_enqueued, 2);
    assert_eq!(completed.events_processed, 2);
    assert_eq!(completed.events_dropped, 0);
    assert_eq!(completed.queue_messages, 0);
    assert_eq!(completed.queue_bytes, 0);
    let output = std::fs::read(fixture.root.path().join("camera.flv")).expect("recorded FLV");
    assert_eq!(
        output[13], 18,
        "metadata must be stored as the first FLV tag"
    );
    server.close(1_721_657_969_400).expect("publisher close");
}

#[test]
fn manual_start_bootstraps_cached_metadata_codec_headers_and_matching_keyframe() {
    let fixture = Fixture::new(RtmpRecorderStart::Manual, limits(), worker_config());
    let (mut server, mut client) = fixture.publisher("camera");
    publish_metadata(&mut client, &mut server, 1_721_657_969_010);
    publish_audio_payload(
        &mut client,
        &mut server,
        0,
        &[0xaf, 0x00, 0x12],
        1_721_657_969_020,
    );
    publish_video_payload(
        &mut client,
        &mut server,
        1,
        &[0x17, 0x00, 0, 0, 0, 0x01],
        1_721_657_969_030,
    );
    publish_video_payload(
        &mut client,
        &mut server,
        2,
        &[0x17, 0x01, 0, 0, 0, 0x22],
        1_721_657_969_040,
    );
    let stream = fixture.registry.snapshot().streams[0].clone();
    let recorder_id = stream.recorders[0].id;

    fixture
        .registry
        .start_recording(stream.id, recorder_id, 1_721_657_969_100)
        .expect("manual start with bootstrap");
    fixture
        .registry
        .stop_recording(stream.id, recorder_id, 1_721_657_969_200)
        .expect("manual stop");
    wait_for_phase(&fixture.registry, |phase| phase == RecorderPhase::Idle);
    wait_for_file(fixture.root.path(), "camera.flv");

    let output = std::fs::read(fixture.root.path().join("camera.flv")).expect("recorded FLV");
    let recorder = &fixture.registry.snapshot().streams[0].recorders[0];
    assert_eq!(recorder.events_enqueued, 4);
    assert!(output.windows(3).any(|window| window == [0xaf, 0x00, 0x12]));
    assert!(
        output
            .windows(6)
            .any(|window| window == [0x17, 0x00, 0, 0, 0, 0x01])
    );
    assert!(
        output
            .windows(6)
            .any(|window| window == [0x17, 0x01, 0, 0, 0, 0x22])
    );
    server.close(1_721_657_969_300).expect("publisher close");
}

#[test]
fn manual_recorder_can_stop_before_the_first_media_event() {
    let fixture = Fixture::new(RtmpRecorderStart::Manual, limits(), worker_config());
    let (mut server, _client) = fixture.publisher("camera");
    let stream = fixture.registry.snapshot().streams[0].clone();
    let recorder_id = stream.recorders[0].id;

    let started = fixture
        .registry
        .start_recording(stream.id, recorder_id, 1_721_657_969_100)
        .expect("manual start");
    assert!(matches!(started.phase, RecorderPhase::Recording { .. }));
    fixture
        .registry
        .stop_recording(stream.id, recorder_id, 1_721_657_969_101)
        .expect("manual stop before media");
    wait_for_phase(&fixture.registry, |phase| phase == RecorderPhase::Idle);
    assert!(!fixture.root.path().join("camera.flv").exists());
    server.close(1_721_657_969_200).expect("publisher close");
}

#[test]
fn manual_capability_blocks_a_real_recorder_without_leaving_a_transition() {
    let fixture = Fixture::with_manual_capability(
        RtmpRecorderStart::Manual,
        limits(),
        worker_config(),
        false,
    );
    let (_server, _client) = fixture.publisher("camera");
    let stream = fixture.registry.snapshot().streams[0].clone();
    let recorder_id = stream.recorders[0].id;

    assert_eq!(
        fixture
            .registry
            .start_recording(stream.id, recorder_id, 1_721_657_969_100),
        Err(CatalogError::RecordingUnavailable)
    );
    assert_eq!(
        fixture.registry.snapshot().streams[0].recorders[0].phase,
        RecorderPhase::Idle
    );
    assert_eq!(fixture.store.stats().active_recorders, 0);
    assert!(!fixture.root.path().join("camera.flv").exists());
}

#[test]
fn recorder_storage_failure_does_not_fail_live_ingest() {
    let fixture = Fixture::new(
        RtmpRecorderStart::Continuous,
        RecordingStoreLimits {
            max_bytes: Some(13),
            max_files: Some(8),
            max_active_recorders: 2,
        },
        worker_config(),
    );
    let (mut server, mut client) = fixture.publisher("camera");

    publish_legacy_audio(&mut client, &mut server, 10, 0x33, 1_721_657_969_100);
    wait_for_phase(&fixture.registry, |phase| {
        matches!(
            phase,
            RecorderPhase::Failed {
                code: RecorderErrorCode::WriteFailed,
                ..
            }
        )
    });
    publish_legacy_audio(&mut client, &mut server, 20, 0x44, 1_721_657_969_200);
    assert_eq!(
        fixture.registry.snapshot().streams[0]
            .media
            .audio
            .payload_bytes_received,
        4
    );
    let recorder = &fixture.registry.snapshot().streams[0].recorders[0];
    assert!(recorder.recoverable_partial_name.is_some());
    assert!(recorder.current_relative_name.is_none());
}

#[test]
fn one_continuous_start_failure_does_not_block_a_sibling_recorder() {
    let failed_root = tempdir().expect("failed recorder root");
    let healthy_root = tempdir().expect("healthy recorder root");
    let failed_store = RecordingStore::open(failed_root.path(), limits()).expect("failed store");
    let healthy_store = RecordingStore::open(healthy_root.path(), limits()).expect("healthy store");
    let mut invalid_worker = worker_config();
    invalid_worker.max_queue_messages = 0;
    let recorders = [
        RtmpRecorderPolicy::new(
            "failed",
            RtmpRecorderStart::Continuous,
            failed_store,
            RecordingPathPolicy::new(".flv", false).expect("failed path policy"),
            invalid_worker,
        ),
        RtmpRecorderPolicy::new(
            "healthy",
            RtmpRecorderStart::Continuous,
            healthy_store,
            RecordingPathPolicy::new(".flv", false).expect("healthy path policy"),
            worker_config(),
        ),
    ];
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::with_recorders(
            "broadcast",
            true,
            true,
            recorders,
        )]),
    );
    let mut server = runtime.session();
    let mut client = connect(&mut server, "broadcast");
    let request = client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("publish request");
    exchange(&mut client, &mut server, vec![request], 1_721_657_969_000);

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams[0].recorders.len(), 2);
    let failed = snapshot.streams[0]
        .recorders
        .iter()
        .find(|recorder| recorder.name.as_deref() == Some("failed"))
        .expect("failed recorder definition");
    assert!(matches!(
        failed.phase,
        RecorderPhase::Failed {
            code: RecorderErrorCode::OpenFailed,
            ..
        }
    ));

    publish_audio(&mut client, &mut server, 10, 0x33, 1_721_657_969_100);
    wait_until(Duration::from_secs(2), || {
        registry.snapshot().streams[0]
            .recorders
            .iter()
            .find(|recorder| recorder.name.as_deref() == Some("healthy"))
            .is_some_and(|recorder| matches!(recorder.phase, RecorderPhase::Recording { .. }))
    });
    server.close(1_721_657_969_200).expect("publisher close");
    wait_for_file(healthy_root.path(), "camera.flv");
    assert!(!failed_root.path().join("camera.flv").exists());
}

#[test]
fn queue_drop_does_not_harm_enhanced_codec_recording() {
    let mut queue_config = worker_config();
    queue_config.max_queue_bytes = 2;
    let queue_fixture = Fixture::new(RtmpRecorderStart::Continuous, limits(), queue_config);
    let (mut queue_server, mut queue_client) = queue_fixture.publisher("queue");
    publish_audio(
        &mut queue_client,
        &mut queue_server,
        0,
        0x55,
        1_721_657_969_100,
    );
    wait_for_phase(&queue_fixture.registry, |phase| {
        matches!(
            phase,
            RecorderPhase::Failed {
                code: RecorderErrorCode::QueueDiscontinuity,
                ..
            }
        )
    });
    let queue_recorder = &queue_fixture.registry.snapshot().streams[0].recorders[0];
    assert_eq!(queue_recorder.discontinuities, 1);
    assert_eq!(queue_recorder.segments_started, 0);

    let codec_fixture = Fixture::new(RtmpRecorderStart::Continuous, limits(), worker_config());
    let (mut codec_server, mut codec_client) = codec_fixture.publisher("enhanced");
    publish_video_payload(
        &mut codec_client,
        &mut codec_server,
        0,
        &[0x90, b'h', b'v', b'c', b'1', 0xaa],
        1_721_657_969_100,
    );
    publish_video_payload(
        &mut codec_client,
        &mut codec_server,
        1,
        &[0x91, b'h', b'v', b'c', b'1', 0xbb],
        1_721_657_969_101,
    );
    publish_video_payload(
        &mut codec_client,
        &mut codec_server,
        2,
        &[0xa1, b'h', b'v', b'c', b'1', 0xcc],
        1_721_657_969_102,
    );
    wait_until(Duration::from_secs(2), || {
        codec_fixture.registry.snapshot().streams[0].recorders[0].bytes_written > 0
    });
    let recorder = &codec_fixture.registry.snapshot().streams[0].recorders[0];
    assert!(matches!(recorder.phase, RecorderPhase::Recording { .. }));
    assert_eq!(recorder.segments_started, 1);
    assert_eq!(
        codec_fixture.registry.snapshot().streams[0]
            .media
            .video
            .payload_bytes_received,
        18
    );
    codec_server
        .close(1_721_657_969_200)
        .expect("publisher close");
    wait_for_file(codec_fixture.root.path(), "enhanced.flv");
}

#[test]
fn stale_manual_ids_never_control_a_replacement_publisher() {
    let fixture = Fixture::new(RtmpRecorderStart::Manual, limits(), worker_config());
    let (mut first_server, _first_client) = fixture.publisher("camera");
    let first = fixture.registry.snapshot().streams[0].clone();
    let first_recorder = first.recorders[0].id;
    let subscriber_id = SessionId::new();
    fixture
        .registry
        .attach_subscriber(
            StreamKey::new("live", "broadcast", "camera"),
            subscriber_id,
            1_721_657_969_050,
        )
        .expect("subscriber keeps logical stream alive");
    first_server.close(1_721_657_969_100).expect("first close");

    let (_replacement_server, _replacement_client) = fixture.publisher("camera");
    let replacement = fixture.registry.snapshot().streams[0].clone();
    assert_eq!(replacement.id, first.id);
    assert_ne!(replacement.recorders[0].id, first_recorder);
    assert!(matches!(
        fixture
            .registry
            .start_recording(first.id, first_recorder, 1_721_657_969_200),
        Err(CatalogError::StreamNotFound(_) | CatalogError::RecorderNotFound { .. })
    ));
    assert_eq!(
        fixture.registry.snapshot().streams[0].recorders[0].phase,
        RecorderPhase::Idle
    );
    fixture
        .registry
        .detach_subscriber(first.id, subscriber_id, 1_721_657_969_300)
        .expect("subscriber detach");
}

#[test]
fn session_drop_hands_stalled_workers_to_the_bounded_runtime_reaper() {
    let mut config = worker_config();
    config.shutdown_timeout = Duration::from_millis(40);
    let fixture = Fixture::new(RtmpRecorderStart::Continuous, limits(), config);
    let (mut server, mut client) = fixture.publisher("camera");
    let ownership = OpenOptions::new()
        .read(true)
        .open(fixture.root.path())
        .expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall storage admission");
    publish_audio(&mut client, &mut server, 0, 0x66, 1_721_657_969_100);
    thread::sleep(Duration::from_millis(20));

    let started = Instant::now();
    drop(server);
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(fixture.registry.snapshot().streams.is_empty());

    flock(&ownership, FlockOperation::Unlock).expect("release storage admission");
    wait_until(Duration::from_secs(2), || {
        fixture.store.stats().active_recorders == 0
    });
}

#[test]
fn active_sessions_retain_the_reaper_after_the_service_runtime_is_dropped() {
    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(root.path(), limits()).expect("recording store");
    let recorder = RtmpRecorderPolicy::new(
        "archive",
        RtmpRecorderStart::Manual,
        store,
        RecordingPathPolicy::new(".flv", false).expect("recording path policy"),
        worker_config(),
    );
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::with_recorders(
            "broadcast",
            true,
            true,
            [recorder],
        )]),
    );
    let mut server = runtime.session();
    drop(runtime);
    let mut client = connect(&mut server, "broadcast");
    let request = client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("publish request");
    exchange(&mut client, &mut server, vec![request], 1_721_657_969_000);
    let stream = registry.snapshot().streams[0].clone();
    let recorder_id = stream.recorders[0].id;
    registry
        .start_recording(stream.id, recorder_id, 1_721_657_969_100)
        .expect("manual start");
    publish_audio(&mut client, &mut server, 10, 0x77, 1_721_657_969_200);
    registry
        .stop_recording(stream.id, recorder_id, 1_721_657_969_300)
        .expect("manual stop");
    wait_for_phase(&registry, |phase| phase == RecorderPhase::Idle);
    wait_for_file(root.path(), "camera.flv");
}

struct Fixture {
    root: TempDir,
    store: RecordingStore,
    registry: Arc<RtmpRegistry>,
    runtime: RtmpServiceRuntime,
}

impl Fixture {
    fn new(
        start: RtmpRecorderStart,
        store_limits: RecordingStoreLimits,
        worker: RecorderWorkerConfig,
    ) -> Self {
        Self::with_manual_capability(start, store_limits, worker, true)
    }

    fn with_manual_capability(
        start: RtmpRecorderStart,
        store_limits: RecordingStoreLimits,
        worker: RecorderWorkerConfig,
        manual_recording: bool,
    ) -> Self {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(root.path(), store_limits).expect("recording store");
        let recorder = RtmpRecorderPolicy::new(
            "archive",
            start,
            store.clone(),
            RecordingPathPolicy::new(".flv", false).expect("recording path policy"),
            worker,
        );
        let application = RtmpApplication::with_recorders("broadcast", true, true, [recorder]);
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording,
        }));
        let runtime = RtmpServiceRuntime::new(
            "live",
            Arc::clone(&registry),
            LiveHub::new(LiveHubLimits {
                max_streams: 4,
                ..LiveHubLimits::default()
            }),
            RtmpSessionPolicy::new([application]),
        );
        Self {
            root,
            store,
            registry,
            runtime,
        }
    }

    fn publisher(&self, stream_name: &str) -> (RtmpSession, ClientSession) {
        let mut server = self.runtime.session();
        let mut client = connect(&mut server, "broadcast");
        let request = client
            .request_publishing(stream_name.into(), PublishRequestType::Live)
            .expect("publish request");
        let events = exchange(&mut client, &mut server, vec![request], 1_721_657_969_000);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
        );
        (server, client)
    }
}

fn limits() -> RecordingStoreLimits {
    RecordingStoreLimits {
        max_bytes: Some(1024 * 1024),
        max_files: Some(32),
        max_active_recorders: 4,
    }
}

fn worker_config() -> RecorderWorkerConfig {
    RecorderWorkerConfig {
        max_queue_messages: 32,
        max_queue_bytes: 1024,
        rotation_interval: None,
        shutdown_timeout: Duration::from_secs(1),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    }
}

fn publish_audio(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    timestamp: u32,
    marker: u8,
    at_unix_ms: u64,
) {
    let packet = client
        .publish_audio_data(
            Bytes::from(vec![0xaf, 0x01, marker]),
            RtmpTimestamp::new(timestamp),
            false,
        )
        .expect("audio packet");
    exchange(client, server, vec![packet], at_unix_ms);
}

fn publish_audio_payload(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    timestamp: u32,
    payload: &[u8],
    at_unix_ms: u64,
) {
    let packet = client
        .publish_audio_data(
            Bytes::copy_from_slice(payload),
            RtmpTimestamp::new(timestamp),
            false,
        )
        .expect("audio packet");
    exchange(client, server, vec![packet], at_unix_ms);
}

fn publish_video_payload(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    timestamp: u32,
    payload: &[u8],
    at_unix_ms: u64,
) {
    let packet = client
        .publish_video_data(
            Bytes::copy_from_slice(payload),
            RtmpTimestamp::new(timestamp),
            false,
        )
        .expect("video packet");
    exchange(client, server, vec![packet], at_unix_ms);
}

fn publish_metadata(client: &mut ClientSession, server: &mut RtmpSession, at_unix_ms: u64) {
    let mut metadata = StreamMetadata::new();
    metadata.encoder = Some("oxiroute-recorder-test".into());
    let packet = client.publish_metadata(&metadata).expect("metadata packet");
    exchange(client, server, vec![packet], at_unix_ms);
}

fn publish_legacy_audio(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    timestamp: u32,
    marker: u8,
    at_unix_ms: u64,
) {
    let packet = client
        .publish_audio_data(
            Bytes::from(vec![0x2f, marker]),
            RtmpTimestamp::new(timestamp),
            false,
        )
        .expect("legacy audio packet");
    exchange(client, server, vec![packet], at_unix_ms);
}

fn wait_for_phase(registry: &RtmpRegistry, predicate: impl Fn(RecorderPhase) -> bool) {
    wait_until(Duration::from_secs(2), || {
        registry
            .snapshot()
            .streams
            .first()
            .and_then(|stream| stream.recorders.first())
            .is_some_and(|recorder| predicate(recorder.phase))
    });
}

fn wait_for_file(root: &std::path::Path, name: &str) {
    wait_until(Duration::from_secs(2), || root.join(name).is_file());
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timeout");
        thread::sleep(Duration::from_millis(5));
    }
}

fn connect(server: &mut RtmpSession, application: &str) -> ClientSession {
    let mut handshake = Handshake::new(PeerType::Client);
    let client_hello = handshake
        .generate_outbound_p0_and_p1()
        .expect("client hello");
    let server_hello = server.receive(&client_hello, 1_000).expect("server hello");
    let client_finish = match handshake
        .process_bytes(&server_hello.concat())
        .expect("client handshake response")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        result @ HandshakeProcessResult::InProgress { .. } => {
            panic!("client handshake did not complete: {result:?}");
        }
    };
    let startup = server
        .receive(&client_finish, 1_000)
        .expect("server handshake completion");
    let (mut client, initial) = ClientSession::new(ClientSessionConfig::new()).expect("client");
    assert!(initial.is_empty());
    assert!(feed_server_packets(&mut client, startup).0.is_empty());
    let request = client
        .request_connection(application.into())
        .expect("connection request");
    let events = exchange(&mut client, server, vec![request], 1_000);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
    );
    client
}

fn exchange(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    initial: Vec<ClientSessionResult>,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    let mut packets = outbound_packets(initial);
    let mut events = Vec::new();
    for _ in 0..8 {
        if packets.is_empty() {
            return events;
        }
        let mut responses = Vec::new();
        while let Some(packet) = packets.pop_front() {
            responses.extend(server.receive(&packet, at_unix_ms).expect("server input"));
        }
        let (next, mut raised) = feed_server_packets(client, responses);
        packets = next;
        events.append(&mut raised);
    }
    panic!("RTMP exchange did not settle");
}

fn feed_server_packets(
    client: &mut ClientSession,
    server_packets: Vec<Vec<u8>>,
) -> (VecDeque<Vec<u8>>, Vec<ClientSessionEvent>) {
    let mut packets = VecDeque::new();
    let mut events = Vec::new();
    for packet in server_packets {
        for result in client.handle_input(&packet).expect("client input") {
            match result {
                ClientSessionResult::OutboundResponse(packet) => packets.push_back(packet.bytes),
                ClientSessionResult::RaisedEvent(event) => events.push(event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
    }
    (packets, events)
}

fn outbound_packets(results: Vec<ClientSessionResult>) -> VecDeque<Vec<u8>> {
    results
        .into_iter()
        .filter_map(|result| match result {
            ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
            ClientSessionResult::RaisedEvent(_)
            | ClientSessionResult::UnhandleableMessageReceived(_) => None,
        })
        .collect()
}
