use std::{
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use super::*;
use crate::{
    HlsFragmentNaming, HlsOutputConfig, LiveHubLimits, MediaApplication, MediaEvent, MediaStore,
    MediaStoreLimits, RtmpAutoPushConfigError, RtmpCapabilities,
};

#[test]
fn preparation_starts_no_runtime_owners_until_consumed() {
    PREPARATION_STARTS.set(0);
    REAPER_STARTS.set(0);
    PULL_STARTS.set(0);
    let hub = LiveHub::new(LiveHubLimits::default());
    let application = RtmpApplication::with_runtime("live", true, false, hub.clone(), [], [])
        .with_pull_targets([RtmpPullTarget {
            address: "127.0.0.1:9".parse().unwrap(),
            host: "127.0.0.1".into(),
            transport: crate::RtmpTransport::Rtmp,
            source_application: "origin".into(),
            source_stream_name: "camera".into(),
            local_application: "live".into(),
            local_stream_name: "camera".into(),
            options: crate::RtmpClientOptions::default(),
            config: crate::RtmpRelayConfig::default(),
        }]);
    let preparation =
        RtmpServicePreparation::new("live", hub, RtmpSessionPolicy::new([application]));

    assert_eq!(PREPARATION_STARTS.get(), 0);
    assert_eq!(REAPER_STARTS.get(), 0);
    assert_eq!(PULL_STARTS.get(), 0);
    let runtime = preparation
        .start(Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        })))
        .expect("start dormant preparation");
    assert_eq!(PREPARATION_STARTS.get(), 1);
    assert_eq!(REAPER_STARTS.get(), 0);
    assert_eq!(PULL_STARTS.get(), 1);
    drop(runtime);
}

fn invalid_auto_push_config() -> RtmpAutoPushConfig {
    RtmpAutoPushConfig {
        enabled: true,
        socket_dir: "relative/socket".into(),
        secret_file: None,
        reconnect_interval: Duration::from_secs(1),
        connect_timeout: Duration::from_secs(1),
        handshake_timeout: Duration::from_secs(1),
        max_peers: 1,
        max_queue_messages: 1,
        max_queue_bytes: 1,
        max_streams: 1,
    }
}

#[test]
fn preparation_rejects_invalid_auto_push_config_before_start() {
    let preparation = RtmpServicePreparation::new(
        "live",
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::default(),
    );
    let Err(error) = preparation.with_auto_push(invalid_auto_push_config()) else {
        panic!("invalid auto-push config was accepted by preparation")
    };
    assert_eq!(error, RtmpAutoPushConfigError::InvalidField("socket_dir"));
}

#[test]
fn runtime_rejects_invalid_auto_push_config_before_coordinator_construction() {
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::default(),
    );
    let Err(error) = runtime.with_auto_push(invalid_auto_push_config()) else {
        panic!("invalid auto-push config was accepted by runtime")
    };
    assert_eq!(error, RtmpAutoPushConfigError::InvalidField("socket_dir"));
}

#[test]
fn retirement_authority_survives_dropped_lifecycle_and_shutdown_handles() {
    use crate::{RecorderWorkerConfig, RecordingPathPolicy, RecordingStore, RecordingStoreLimits};
    use tempfile::tempdir;

    let root = tempdir().expect("recording root");
    let store = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024),
            max_files: Some(4),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let recorder = RtmpRecorderPolicy::new(
        "archive",
        RtmpRecorderStart::Continuous,
        store,
        RecordingPathPolicy::new(".flv", false).expect("recording path policy"),
        RecorderWorkerConfig::default(),
    );
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::with_runtime(
            "live",
            true,
            false,
            LiveHub::new(LiveHubLimits::default()),
            [],
            [recorder],
        )]),
    );
    let callback_count = runtime.retirement_callback_count();
    let lifecycle = runtime.recorder_lifecycle().expect("recorder lifecycle");
    let shutdown = lifecycle.initiate_shutdown(Instant::now() + Duration::from_secs(1));
    drop(shutdown);
    drop(lifecycle);
    drop(runtime);
    drop(registry);

    let deadline = Instant::now() + Duration::from_secs(1);
    while callback_count.load(Ordering::Acquire) == 0 {
        assert!(
            Instant::now() < deadline,
            "recording retirement callback timed out"
        );
        thread::yield_now();
    }

    let reopened = RecordingStore::open(
        root.path(),
        RecordingStoreLimits {
            max_bytes: Some(1024),
            max_files: Some(4),
            max_active_recorders: 1,
        },
    )
    .expect("retired store reopens");
    drop(reopened);
    assert_eq!(callback_count.load(Ordering::Acquire), 1);
}

#[test]
fn publisher_reconnect_never_exposes_split_hub_and_catalog_ownership() {
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::default(),
    );
    let key = StreamKey::new("live", "broadcast", "camera");
    let barrier = Arc::new(Barrier::new(2));

    let writer_runtime = runtime.clone();
    let writer_key = key.clone();
    let writer_barrier = Arc::clone(&barrier);
    let writer = thread::spawn(move || {
        writer_barrier.wait();
        for at_unix_ms in 1..=1_000 {
            let role = writer_runtime
                .acquire_publisher_role(writer_key.clone(), SessionId::new(), at_unix_ms)
                .expect("publisher role");
            assert_eq!(writer_runtime.publisher_presence(&writer_key), (true, true));
            drop(role);
            assert_eq!(
                writer_runtime.publisher_presence(&writer_key),
                (false, false)
            );
        }
    });

    barrier.wait();
    for _ in 0..2_000 {
        let (hub, catalog) = runtime.publisher_presence(&key);
        assert_eq!(hub, catalog);
    }
    writer.join().expect("publisher reconnect thread");
    assert_eq!(runtime.publisher_presence(&key), (false, false));
}

#[test]
fn concurrent_stale_takeover_never_exposes_split_hub_and_catalog_ownership() {
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::default(),
    );
    let key = StreamKey::new("live", "broadcast", "camera");
    let incumbent = runtime
        .acquire_publisher_role(key.clone(), SessionId::new(), 1)
        .expect("incumbent publisher");
    let barrier = Arc::new(Barrier::new(3));

    let first_runtime = runtime.clone();
    let first_key = key.clone();
    let first_barrier = Arc::clone(&barrier);
    let first = thread::spawn(move || {
        first_barrier.wait();
        first_runtime.acquire_publisher_role(
            first_key,
            SessionId::new(),
            1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS,
        )
    });
    let second_runtime = runtime.clone();
    let second_key = key.clone();
    let second_barrier = Arc::clone(&barrier);
    let second = thread::spawn(move || {
        second_barrier.wait();
        second_runtime.acquire_publisher_role(
            second_key,
            SessionId::new(),
            1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS,
        )
    });

    barrier.wait();
    for _ in 0..2_000 {
        let presence = runtime.publisher_presence(&key);
        assert_eq!(presence.0, presence.1);
    }

    let first_result = first.join().expect("first takeover thread");
    let second_result = second.join().expect("second takeover thread");
    let ((Ok(winner), Err(loser)) | (Err(loser), Ok(winner))) = (first_result, second_result)
    else {
        panic!("concurrent takeover did not produce one winner and one rejection")
    };
    assert!(matches!(
        loser,
        PublisherRoleError::Hub(LiveHubError::PublisherAlreadyAttached { .. })
    ));
    assert_eq!(runtime.publisher_presence(&key), (true, true));

    drop(incumbent);
    assert_eq!(runtime.publisher_presence(&key), (true, true));
    drop(winner);
    assert_eq!(runtime.publisher_presence(&key), (false, false));
}

#[test]
fn takeover_expires_the_old_hub_lease_before_expected_catalog_detach() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let hub = LiveHub::new(LiveHubLimits::default());
    let key = StreamKey::new("live", "broadcast", "camera");
    let old_session_id = SessionId::new();
    let old_lease = hub
        .attach_publisher(key.clone())
        .expect("old hub publisher");
    registry
        .attach_publisher(key.clone(), old_session_id, Vec::new(), 1)
        .expect("old catalog publisher");
    let owner = registry
        .stale_publisher_owner(
            &key,
            1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS,
            RTMP_STALE_PUBLISHER_THRESHOLD_MS,
        )
        .expect("stale catalog owner");

    let new_lease = hub
        .replace_publisher(key.clone())
        .expect("replacement hub publisher");
    let event =
        MediaEvent::audio(0, Arc::<[u8]>::from(&[0xaf, 0x01, 0x11][..])).expect("audio event");
    assert!(matches!(
        old_lease.publish(event.clone()),
        Err(LiveHubError::PublisherExpired { .. })
    ));

    let shutdown = registry
        .detach_expected_publisher(owner, 1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS)
        .expect("expected catalog detach");
    shutdown.shutdown(1 + RTMP_STALE_PUBLISHER_THRESHOLD_MS);
    new_lease.publish(event).expect("replacement media");
    drop(old_lease);
    assert!(hub.has_publisher(&key));
}

#[test]
fn existing_session_cannot_register_after_runtime_admission_closes() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::default(),
    );
    let admitted_session = runtime.clone();
    let key = StreamKey::new("live", "broadcast", "camera");

    runtime.close_admission();

    assert!(matches!(
        admitted_session.acquire_publisher_role(key.clone(), SessionId::new(), 1),
        Err(PublisherRoleError::AdmissionClosed)
    ));
    assert!(matches!(
        registry.register_publisher(key, SessionId::new(), Vec::new(), 1),
        Err(CatalogError::AdmissionClosed)
    ));
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn access_policy_uses_the_first_matching_rule_and_requires_the_configured_token() {
    let policy = RtmpAccessPolicy::new(
        [
            RtmpAccessRule::new(
                RtmpAccessAction::Deny,
                RtmpNetwork::parse("192.0.2.0/24").expect("valid network"),
            ),
            RtmpAccessRule::new(RtmpAccessAction::Allow, RtmpNetwork::All),
        ],
        Some(RtmpTokenPolicy::stream_query("token", "secret").expect("valid token policy")),
    );

    assert_eq!(
        policy.authorize(
            Some("192.0.2.10".parse().expect("valid peer")),
            Some("token=secret")
        ),
        Err(RtmpAuthorizationError::NetworkDenied)
    );
    assert_eq!(
        policy.authorize(
            Some("198.51.100.10".parse().expect("valid peer")),
            Some("token=wrong")
        ),
        Err(RtmpAuthorizationError::TokenRejected)
    );
    assert_eq!(
        policy.authorize(
            Some("198.51.100.10".parse().expect("valid peer")),
            Some("token=secret")
        ),
        Ok(())
    );
}

#[test]
fn application_session_ceilings_are_shared_and_released() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let application = RtmpApplication::new("broadcast", true, true).with_authorization(
        RtmpAccessPolicy::default(),
        RtmpAccessPolicy::default(),
        RtmpSessionCeilings::new(1, 1, 1),
    );
    let runtime = RtmpServiceRuntime::new(
        "live",
        registry,
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([application]),
    );

    let connection = runtime
        .acquire_connection("broadcast")
        .expect("first connection lease");
    assert!(matches!(
        runtime.acquire_connection("broadcast"),
        Err(SessionLimitError {
            counter: SessionCounter::Connections,
            ..
        })
    ));
    drop(connection);

    let key = StreamKey::new("live", "broadcast", "camera");
    let publisher = runtime
        .acquire_publisher_role(key.clone(), SessionId::new(), 1)
        .expect("first publisher lease");
    assert!(matches!(
        runtime.acquire_publisher_role(key.clone(), SessionId::new(), 2),
        Err(PublisherRoleError::SessionLimit(SessionLimitError {
            counter: SessionCounter::Publishers,
            ..
        }))
    ));
    drop(publisher);

    let viewer = runtime
        .acquire_playback_role(key.clone(), SessionId::new(), 1, true, 3)
        .expect("first viewer lease");
    assert!(matches!(
        runtime.acquire_playback_role(key, SessionId::new(), 1, true, 4),
        Err(PlaybackRoleError::SessionLimit(SessionLimitError {
            counter: SessionCounter::Viewers,
            ..
        }))
    ));
    drop(viewer);
}

#[test]
fn media_store_limit_does_not_reject_publisher() {
    let directory = tempfile::tempdir().expect("temporary media directory");
    let store = Arc::new(
        MediaStore::open(
            directory.path(),
            MediaStoreLimits {
                max_bytes: 1024 * 1024,
                max_files: 16,
                max_active_streams: 1,
                max_file_bytes: 1024 * 1024,
            },
        )
        .expect("media store"),
    );
    let media = Arc::new(MediaApplication::new(Some(Arc::new(HlsOutputConfig {
        store,
        segment_duration: Duration::from_secs(1),
        max_segment_duration: Duration::from_secs(2),
        playlist_length: Duration::from_secs(6),
        naming: HlsFragmentNaming::Sequential,
        nested: false,
        cleanup: true,
        variants: Vec::new(),
        keys: None,
        max_segment_bytes: 1024 * 1024,
        max_queue_messages: 8,
    }))));
    let application = RtmpApplication::new("broadcast", true, true).with_media(Some(media));
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([application]),
    );

    let first_key = StreamKey::new("live", "broadcast", "first");
    let first = runtime
        .acquire_publisher_role(first_key, SessionId::new(), 1)
        .expect("first publisher");
    let second_key = StreamKey::new("live", "broadcast", "second");
    let second = runtime
        .acquire_publisher_role(second_key.clone(), SessionId::new(), 2)
        .expect("media failure must not reject publisher");

    assert_eq!(runtime.publisher_presence(&second_key), (true, true));
    drop(second);
    drop(first);
}

#[cfg(unix)]
#[test]
fn auto_push_transport_starts_only_when_a_publisher_is_admitted() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("auto-push socket directory");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .expect("secure auto-push socket directory");
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::default(),
    )
    .with_auto_push(RtmpAutoPushConfig {
        enabled: true,
        socket_dir: directory.path().to_path_buf(),
        secret_file: None,
        reconnect_interval: Duration::from_millis(100),
        connect_timeout: Duration::from_millis(500),
        handshake_timeout: Duration::from_millis(500),
        max_peers: 2,
        max_queue_messages: 16,
        max_queue_bytes: 1024 * 1024,
        max_streams: 2,
    })
    .expect("auto-push runtime");

    assert!(!runtime.auto_push_status().started);
    let role = runtime
        .acquire_publisher_role(
            StreamKey::new("live", "broadcast", "camera"),
            SessionId::new(),
            1,
        )
        .expect("publisher role");
    assert!(runtime.auto_push_status().started);
    assert_eq!(runtime.auto_push_status().source_streams, 1);
    drop(role);
    assert_eq!(runtime.auto_push_status().source_streams, 0);
    runtime.close_admission();
}
