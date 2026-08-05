use std::{
    collections::VecDeque,
    fs,
    net::IpAddr,
    sync::Arc,
    thread,
    time::Duration,
};

use bytes::Bytes;
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MAX_RTMP_APPLICATION_BYTES, MAX_RTMP_STREAM_NAME_BYTES,
    RtmpAccessAction, RtmpAccessPolicy, RtmpAccessRule, RtmpApplication, RtmpCapabilities,
    RtmpNetwork, RtmpOutboundPolicy, RtmpRegistry, RtmpServiceRuntime, RtmpSession,
    RtmpSessionCeilings, RtmpSessionError, RtmpSessionPolicy, RtmpTokenPolicy, StreamMetadata,
    VodApplication, VodLimits, VodSourceDefinition,
};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    rml_amf0::Amf0Value,
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
    },
    time::RtmpTimestamp,
};

#[test]
fn idle_viewer_receives_metadata_headers_and_keyframe_in_wire_order() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    let mut viewer_server = server(&registry, hub.clone(), &policy);
    let mut viewer = connect(&mut viewer_server, "live");
    let play = viewer
        .request_playback("camera?viewer=one".into())
        .expect("play request");
    let events = exchange(&mut viewer, &mut viewer_server, vec![play], 1_100);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PlaybackRequestAccepted))
    );
    let waiting = registry.snapshot();
    assert_eq!(waiting.streams.len(), 1);
    assert_eq!(waiting.streams[0].subscriber_count, 1);
    assert!(waiting.streams[0].publisher.is_none());

    let mut publisher_server = server(&registry, hub, &policy);
    let mut publisher = connect(&mut publisher_server, "live");
    request_publish(
        &mut publisher,
        &mut publisher_server,
        "camera?token=publish",
        1_200,
    );
    publish_metadata(&mut publisher, &mut publisher_server, 1_300);
    publish_audio(
        &mut publisher,
        &mut publisher_server,
        10,
        &[0xaf, 0x00, 0x12],
    );
    publish_video(
        &mut publisher,
        &mut publisher_server,
        11,
        &[0x17, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    publish_video(
        &mut publisher,
        &mut publisher_server,
        12,
        &[0x27, 0x01, 0x00, 0x00, 0x00, 0x22],
    );
    assert!(
        viewer_server
            .drain_playback(32)
            .expect("empty drain")
            .is_empty()
    );
    publish_video(
        &mut publisher,
        &mut publisher_server,
        13,
        &[0x17, 0x01, 0x00, 0x00, 0x00, 0x33],
    );

    let packets = viewer_server.drain_playback(32).expect("playback drain");
    let (_, events) = feed_server_packets(&mut viewer, packets);
    assert_eq!(
        event_labels(&events),
        ["metadata", "audio", "video", "video"]
    );
    let ClientSessionEvent::StreamMetadataReceived { metadata } = &events[0] else {
        panic!("metadata must be first");
    };
    assert_eq!(metadata.encoder.as_deref(), Some("oxiroute-wire-test"));
    assert!(matches!(
        &events[1],
        ClientSessionEvent::AudioDataReceived { timestamp, data }
            if timestamp.value == 10 && data.as_ref() == [0xaf, 0x00, 0x12]
    ));
    assert!(matches!(
        &events[2],
        ClientSessionEvent::VideoDataReceived { timestamp, data }
            if timestamp.value == 11 && data.as_ref() == [0x17, 0x00, 0x00, 0x00, 0x00, 0x01]
    ));
    assert!(matches!(
        &events[3],
        ClientSessionEvent::VideoDataReceived { timestamp, data }
            if timestamp.value == 13 && data.as_ref() == [0x17, 0x01, 0x00, 0x00, 0x00, 0x33]
    ));
    assert_eq!(registry.snapshot().streams[0].subscriber_count, 1);

    let stop = viewer.stop_playback().expect("stop playback");
    exchange(&mut viewer, &mut viewer_server, stop, 1_400);
    let stopped = registry.snapshot();
    assert_eq!(stopped.streams[0].subscriber_count, 0);
    assert!(stopped.streams[0].publisher.is_some());
}

#[test]
fn vod_playback_emits_flv_media_and_completes() {
    let directory = tempfile::tempdir().expect("VOD directory");
    fs::write(directory.path().join("movie.flv"), test_flv()).expect("VOD object");
    let vod = Arc::new(
        VodApplication::new(
            "edge",
            "vod",
            VodLimits {
                max_sessions: 1,
                max_file_bytes: 1024,
                max_duration: Duration::from_secs(60),
            },
            [VodSourceDefinition::Local {
                name: "archive".into(),
                root_directory: directory.path().to_path_buf(),
            }],
            &RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
        )
        .expect("VOD application"),
    );
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let application = RtmpApplication::new("vod", false, false).with_vod(Some(vod));
    let runtime = RtmpServiceRuntime::new(
        "edge",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([application]),
    );
    let mut server = runtime.session();
    let mut client = connect(&mut server, "vod");
    let play = client
        .request_playback("archive/movie.flv".into())
        .expect("VOD play request");
    let accepted = exchange(&mut client, &mut server, vec![play], 9_000);
    assert!(accepted
        .iter()
        .any(|event| matches!(event, ClientSessionEvent::PlaybackRequestAccepted)));

    let mut received = Vec::new();
    let mut completed = false;
    for _ in 0..100 {
        let packets = server.drain_playback(32).expect("VOD playback drain");
        if packets.is_empty() {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let (_, events) = feed_server_packets(&mut client, packets);
        completed |= has_status(&events, "NetStream.Play.Complete");
        received.extend(events);
        if completed {
            break;
        }
    }
    assert!(completed, "VOD playback did not complete");
    assert_eq!(event_labels(&received), ["audio", "video", "status"]);
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn late_viewer_waits_for_a_future_keyframe_and_drain_turns_are_bounded() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    let mut publisher_server = server(&registry, hub.clone(), &policy);
    let mut publisher = connect(&mut publisher_server, "live");
    request_publish(&mut publisher, &mut publisher_server, "camera", 2_000);
    publish_metadata(&mut publisher, &mut publisher_server, 2_010);
    publish_audio(
        &mut publisher,
        &mut publisher_server,
        1,
        &[0xaf, 0x00, 0x12],
    );
    publish_video(
        &mut publisher,
        &mut publisher_server,
        2,
        &[0x17, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    publish_video(
        &mut publisher,
        &mut publisher_server,
        3,
        &[0x17, 0x01, 0x00, 0x00, 0x00, 0x03],
    );

    let mut viewer_server = server(&registry, hub, &policy);
    let mut viewer = connect(&mut viewer_server, "live");
    let play = viewer
        .request_playback("camera".into())
        .expect("play request");
    exchange(&mut viewer, &mut viewer_server, vec![play], 2_100);
    publish_audio(
        &mut publisher,
        &mut publisher_server,
        4,
        &[0xaf, 0x01, 0x04],
    );
    publish_video(
        &mut publisher,
        &mut publisher_server,
        5,
        &[0x27, 0x01, 0x00, 0x00, 0x00, 0x05],
    );
    assert!(
        viewer_server
            .drain_playback(3)
            .expect("gated drain")
            .is_empty()
    );
    publish_video(
        &mut publisher,
        &mut publisher_server,
        6,
        &[0x17, 0x01, 0x00, 0x00, 0x00, 0x06],
    );

    let first_turn = viewer_server.drain_playback(3).expect("first drain turn");
    assert_eq!(first_turn.len(), 3);
    let (_, first_events) = feed_server_packets(&mut viewer, first_turn);
    assert_eq!(event_labels(&first_events), ["metadata", "audio", "video"]);
    let second_turn = viewer_server.drain_playback(3).expect("second drain turn");
    assert_eq!(second_turn.len(), 1);
    let (_, second_events) = feed_server_packets(&mut viewer, second_turn);
    assert_eq!(event_labels(&second_events), ["video"]);
    assert!(
        viewer_server
            .drain_playback(0)
            .expect("zero drain")
            .is_empty()
    );

    for timestamp in 10..50 {
        publish_audio(
            &mut publisher,
            &mut publisher_server,
            timestamp,
            &[0xaf, 0x01, 0x44],
        );
    }
    let bounded = viewer_server
        .drain_playback(usize::MAX)
        .expect("internally bounded drain");
    assert_eq!(bounded.len(), 32);
    feed_server_packets(&mut viewer, bounded);
    assert_eq!(
        viewer_server
            .drain_playback(usize::MAX)
            .expect("remaining drain")
            .len(),
        8
    );
}

#[test]
fn restart_clears_queued_media_and_viewer_detaches_cleanly() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    let mut viewer_server = server(&registry, hub.clone(), &policy);
    let mut viewer = connect(&mut viewer_server, "live");
    let play = viewer
        .request_playback("camera".into())
        .expect("play request");
    exchange(&mut viewer, &mut viewer_server, vec![play], 3_000);

    let mut first_server = server(&registry, hub.clone(), &policy);
    let mut first = connect(&mut first_server, "live");
    request_publish(&mut first, &mut first_server, "camera", 3_100);
    publish_metadata(&mut first, &mut first_server, 3_110);
    publish_video(
        &mut first,
        &mut first_server,
        1,
        &[0x17, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    publish_video(
        &mut first,
        &mut first_server,
        2,
        &[0x17, 0x01, 0x00, 0x00, 0x00, 0x02],
    );
    first_server.close(3_200).expect("first publisher close");
    assert!(
        viewer_server
            .drain_playback(32)
            .expect("reset drain")
            .is_empty()
    );

    let mut second_server = server(&registry, hub, &policy);
    let mut second = connect(&mut second_server, "live");
    request_publish(&mut second, &mut second_server, "camera", 3_300);
    publish_video(
        &mut second,
        &mut second_server,
        3,
        &[0x27, 0x01, 0x00, 0x00, 0x00, 0x03],
    );
    assert!(
        viewer_server
            .drain_playback(32)
            .expect("gated drain")
            .is_empty()
    );
    publish_video(
        &mut second,
        &mut second_server,
        4,
        &[0x17, 0x01, 0x00, 0x00, 0x00, 0x04],
    );
    let packets = viewer_server.drain_playback(32).expect("restart drain");
    let (_, events) = feed_server_packets(&mut viewer, packets);
    assert_eq!(event_labels(&events), ["video"]);

    viewer_server.close(3_400).expect("viewer close");
    assert_eq!(registry.snapshot().streams[0].subscriber_count, 0);
    second_server.close(3_500).expect("second publisher close");
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn rejects_idle_viewers_and_capacity_with_stable_status_codes() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), false);
    let mut no_idle_server = server(&registry, hub, &policy);
    let mut no_idle_client = connect(&mut no_idle_server, "live");
    let play = no_idle_client
        .request_playback("camera".into())
        .expect("play request");
    let events = exchange(&mut no_idle_client, &mut no_idle_server, vec![play], 4_000);
    assert!(
        has_status(&events, "NetStream.Play.StreamNotFound"),
        "unexpected events: {events:?}"
    );

    let limits = LiveHubLimits {
        max_subscribers: 0,
        ..LiveHubLimits::default()
    };
    let (registry, hub, policy) = runtime(limits, true);
    let mut capped_server = server(&registry, hub, &policy);
    let mut capped_client = connect(&mut capped_server, "live");
    let play = capped_client
        .request_playback("camera".into())
        .expect("play request");
    let events = exchange(&mut capped_client, &mut capped_server, vec![play], 4_100);
    assert!(has_status(&events, "NetStream.Play.Failed"));
    assert!(registry.snapshot().streams.is_empty());

    let limits = LiveHubLimits {
        max_streams: 0,
        ..LiveHubLimits::default()
    };
    let (registry, hub, policy) = runtime(limits, true);
    let mut capped_server = server(&registry, hub, &policy);
    let mut capped_client = connect(&mut capped_server, "live");
    let publish = capped_client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("publish request");
    let events = exchange(&mut capped_client, &mut capped_server, vec![publish], 4_200);
    assert!(has_status(&events, "NetStream.Publish.BadName"));
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn application_identity_is_exact() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    let mut server = server(&registry, hub, &policy);
    let mut client = handshake(&mut server);
    let request = client
        .request_connection("Live".into())
        .expect("connection request");
    let events = exchange(&mut client, &mut server, vec![request], 5_000);
    assert!(events.iter().any(|event| matches!(
        event,
        ClientSessionEvent::ConnectionRequestRejected { description }
            if description == "RTMP application is not configured"
    )));
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn rejects_oversized_connection_and_playback_identities_with_stable_statuses() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    let mut oversized_server = server(&registry, hub.clone(), &policy);
    let mut client = handshake(&mut oversized_server);
    let request = client
        .request_connection("a".repeat(MAX_RTMP_APPLICATION_BYTES + 1))
        .expect("oversized connection request");
    let events = exchange(&mut client, &mut oversized_server, vec![request], 5_100);
    assert!(events.iter().any(|event| matches!(
        event,
        ClientSessionEvent::ConnectionRequestRejected { description }
            if description == "RTMP application exceeds the configured byte limit"
    )));

    let mut server = server(&registry, hub, &policy);
    let mut client = connect(&mut server, "live");
    let request = client
        .request_playback("s".repeat(MAX_RTMP_STREAM_NAME_BYTES + 1))
        .expect("oversized play request");
    let events = exchange(&mut client, &mut server, vec![request], 5_200);
    assert!(has_status(&events, "NetStream.Play.Failed"));
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn one_service_runtime_bridges_publish_and_play_across_listeners() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let runtime = RtmpServiceRuntime::new(
        "shared-live-service",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::new("live", true, false)]),
    );
    let mut publisher_server = runtime.session();
    let mut publisher = connect(&mut publisher_server, "live");
    request_publish(&mut publisher, &mut publisher_server, "camera", 6_000);

    let mut viewer_server = runtime.session();
    let mut viewer = connect(&mut viewer_server, "live");
    let play = viewer
        .request_playback("camera".into())
        .expect("play request on second listener");
    let events = exchange(&mut viewer, &mut viewer_server, vec![play], 6_100);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PlaybackRequestAccepted))
    );

    publish_video(
        &mut publisher,
        &mut publisher_server,
        1,
        &[0x17, 0x01, 0x00, 0x00, 0x00, 0xaa],
    );
    let packets = viewer_server
        .drain_playback(32)
        .expect("cross-listener drain");
    let (_, events) = feed_server_packets(&mut viewer, packets);
    assert_eq!(event_labels(&events), ["video"]);
    assert_eq!(registry.snapshot().streams.len(), 1);
    assert_eq!(
        registry.snapshot().streams[0].key.server_id,
        "shared-live-service"
    );
}

#[test]
fn viewer_ceiling_rejects_a_second_playback_until_the_first_role_closes() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let hub = LiveHub::new(LiveHubLimits::default());
    let application = RtmpApplication::new("live", true, false).with_authorization(
        RtmpAccessPolicy::default(),
        RtmpAccessPolicy::default(),
        RtmpSessionCeilings::new(16, 16, 1),
    );
    let runtime = RtmpServiceRuntime::new(
        "shared-live-service",
        Arc::clone(&registry),
        hub,
        RtmpSessionPolicy::new([application]),
    );

    let mut publisher_server = runtime.session();
    let mut publisher = connect(&mut publisher_server, "live");
    request_publish(&mut publisher, &mut publisher_server, "camera", 6_000);

    let mut first_viewer_server = runtime.session();
    let mut first_viewer = connect(&mut first_viewer_server, "live");
    let first_play = first_viewer
        .request_playback("camera".into())
        .expect("first play request");
    let first_events = exchange(
        &mut first_viewer,
        &mut first_viewer_server,
        vec![first_play],
        6_100,
    );
    assert!(
        first_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PlaybackRequestAccepted))
    );

    let mut second_viewer_server = runtime.session();
    let mut second_viewer = connect(&mut second_viewer_server, "live");
    let second_play = second_viewer
        .request_playback("camera".into())
        .expect("second play request");
    let second_events = exchange(
        &mut second_viewer,
        &mut second_viewer_server,
        vec![second_play],
        6_200,
    );
    assert!(has_status(&second_events, "NetStream.Play.Failed"));

    first_viewer_server
        .close(6_300)
        .expect("close first viewer role");
    drop(second_viewer);
    drop(second_viewer_server);
    let mut retry_server = runtime.session();
    let mut retry_client = connect(&mut retry_server, "live");
    let retry = retry_client
        .request_playback("camera".into())
        .expect("retry play request");
    let retry_events = exchange(&mut retry_client, &mut retry_server, vec![retry], 6_400);
    assert!(
        retry_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PlaybackRequestAccepted))
    );
}

#[test]
fn enforces_ordered_play_acl_and_stream_query_token() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let hub = LiveHub::new(LiveHubLimits::default());
    let play = RtmpAccessPolicy::new(
        [
            RtmpAccessRule::new(
                RtmpAccessAction::Deny,
                RtmpNetwork::parse("192.0.2.0/24").expect("valid network"),
            ),
            RtmpAccessRule::new(RtmpAccessAction::Allow, RtmpNetwork::All),
        ],
        Some(RtmpTokenPolicy::stream_query("viewer", "secret").expect("valid token policy")),
    );
    let application = RtmpApplication::new("live", true, false).with_authorization(
        RtmpAccessPolicy::default(),
        play,
        RtmpSessionCeilings::default(),
    );
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        hub,
        RtmpSessionPolicy::new([application]),
    );

    let mut publisher_server = runtime.session();
    let mut publisher = connect(&mut publisher_server, "live");
    request_publish(&mut publisher, &mut publisher_server, "camera", 8_000);

    let mut denied_server = runtime.session_with_peer_addr(Some(
        "192.0.2.10".parse::<IpAddr>().expect("valid peer address"),
    ));
    let mut denied_client = connect(&mut denied_server, "live");
    let denied_play = denied_client
        .request_playback("camera?viewer=secret".into())
        .expect("denied play request");
    let denied_events = exchange(
        &mut denied_client,
        &mut denied_server,
        vec![denied_play],
        8_100,
    );
    assert!(has_status(&denied_events, "NetStream.Play.Failed"));

    let mut wrong_server = runtime.session_with_peer_addr(Some(
        "198.51.100.10"
            .parse::<IpAddr>()
            .expect("valid peer address"),
    ));
    let mut wrong_client = connect(&mut wrong_server, "live");
    let wrong_play = wrong_client
        .request_playback("camera?viewer=wrong".into())
        .expect("wrong token play request");
    let wrong_events = exchange(
        &mut wrong_client,
        &mut wrong_server,
        vec![wrong_play],
        8_200,
    );
    assert!(has_status(&wrong_events, "NetStream.Play.Failed"));

    let mut accepted_server = runtime.session_with_peer_addr(Some(
        "198.51.100.10"
            .parse::<IpAddr>()
            .expect("valid peer address"),
    ));
    let mut accepted_client = connect(&mut accepted_server, "live");
    let accepted_play = accepted_client
        .request_playback("camera?viewer=secret".into())
        .expect("accepted play request");
    let accepted_events = exchange(
        &mut accepted_client,
        &mut accepted_server,
        vec![accepted_play],
        8_300,
    );
    assert!(
        accepted_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PlaybackRequestAccepted))
    );
}

#[test]
fn dropping_sessions_unregisters_catalog_and_hub_roles() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    {
        let mut publisher_server = server(&registry, hub.clone(), &policy);
        let mut publisher = connect(&mut publisher_server, "live");
        request_publish(&mut publisher, &mut publisher_server, "camera", 7_000);
        assert_eq!(registry.snapshot().streams.len(), 1);
        assert_eq!(hub.stats().publishers, 1);
    }
    assert!(registry.snapshot().streams.is_empty());
    assert_eq!(hub.stats().publishers, 0);

    let mut publisher_server = server(&registry, hub.clone(), &policy);
    let mut publisher = connect(&mut publisher_server, "live");
    request_publish(&mut publisher, &mut publisher_server, "camera", 7_100);
    {
        let mut viewer_server = server(&registry, hub.clone(), &policy);
        let mut viewer = connect(&mut viewer_server, "live");
        let play = viewer
            .request_playback("camera".into())
            .expect("play request");
        exchange(&mut viewer, &mut viewer_server, vec![play], 7_200);
        assert_eq!(registry.snapshot().streams[0].subscriber_count, 1);
        assert_eq!(hub.stats().subscribers, 1);
    }
    assert_eq!(registry.snapshot().streams[0].subscriber_count, 0);
    assert_eq!(hub.stats().subscribers, 0);
}

#[test]
fn playback_drain_rejects_sessions_without_an_active_viewer() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    let mut session = server(&registry, hub, &policy);

    assert!(matches!(
        session.drain_playback(1),
        Err(RtmpSessionError::NoActivePlayback)
    ));
}

#[test]
fn playback_polling_state_excludes_idle_and_publisher_sessions() {
    let (registry, hub, policy) = runtime(LiveHubLimits::default(), true);
    let mut publisher_server = server(&registry, hub.clone(), &policy);
    assert!(!publisher_server.is_playback_active());
    let mut publisher = connect(&mut publisher_server, "live");
    assert!(!publisher_server.is_playback_active());
    request_publish(&mut publisher, &mut publisher_server, "camera", 7_300);
    assert!(!publisher_server.is_playback_active());

    let mut viewer_server = server(&registry, hub, &policy);
    let mut viewer = connect(&mut viewer_server, "live");
    let play = viewer
        .request_playback("camera".into())
        .expect("play request");
    exchange(&mut viewer, &mut viewer_server, vec![play], 7_400);

    assert!(viewer_server.is_playback_active());
}

fn runtime(
    limits: LiveHubLimits,
    idle_streams: bool,
) -> (Arc<RtmpRegistry>, LiveHub, RtmpSessionPolicy) {
    (
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(limits),
        RtmpSessionPolicy::new([RtmpApplication::new("live", true, idle_streams)]),
    )
}

fn server(registry: &Arc<RtmpRegistry>, hub: LiveHub, policy: &RtmpSessionPolicy) -> RtmpSession {
    RtmpSession::new("edge", Arc::clone(registry), hub, policy.clone())
}

fn connect(server: &mut RtmpSession, application: &str) -> ClientSession {
    let mut client = handshake(server);
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

fn handshake(server: &mut RtmpSession) -> ClientSession {
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
    client
}

fn request_publish(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    name: &str,
    at_unix_ms: u64,
) {
    let request = client
        .request_publishing(name.into(), PublishRequestType::Live)
        .expect("publish request");
    let events = exchange(client, server, vec![request], at_unix_ms);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
}

fn publish_metadata(client: &mut ClientSession, server: &mut RtmpSession, at_unix_ms: u64) {
    let mut metadata = StreamMetadata::new();
    metadata.video_codec_id = Some(7);
    metadata.audio_codec_id = Some(10);
    metadata.encoder = Some("oxiroute-wire-test".into());
    let packet = client
        .publish_metadata(&metadata)
        .expect("publish metadata");
    exchange(client, server, vec![packet], at_unix_ms);
}

fn publish_audio(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    timestamp_ms: u32,
    payload: &'static [u8],
) {
    let packet = client
        .publish_audio_data(
            Bytes::from_static(payload),
            RtmpTimestamp::new(timestamp_ms),
            false,
        )
        .expect("publish audio");
    exchange(
        client,
        server,
        vec![packet],
        u64::from(timestamp_ms) + 10_000,
    );
}

fn publish_video(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    timestamp_ms: u32,
    payload: &'static [u8],
) {
    let packet = client
        .publish_video_data(
            Bytes::from_static(payload),
            RtmpTimestamp::new(timestamp_ms),
            payload[0] >> 4 == 3,
        )
        .expect("publish video");
    exchange(
        client,
        server,
        vec![packet],
        u64::from(timestamp_ms) + 10_000,
    );
}

fn exchange(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    initial_results: Vec<ClientSessionResult>,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    let mut client_packets = outbound_packets(initial_results);
    let mut events = Vec::new();
    for _ in 0..8 {
        if client_packets.is_empty() {
            return events;
        }
        let mut server_packets = Vec::new();
        while let Some(packet) = client_packets.pop_front() {
            server_packets.extend(server.receive(&packet, at_unix_ms).expect("server input"));
        }
        let (next_packets, mut next_events) = feed_server_packets(client, server_packets);
        client_packets = next_packets;
        events.append(&mut next_events);
    }
    panic!("RTMP exchange did not settle");
}

fn feed_server_packets(
    client: &mut ClientSession,
    packets: Vec<Vec<u8>>,
) -> (VecDeque<Vec<u8>>, Vec<ClientSessionEvent>) {
    let mut outbound = VecDeque::new();
    let mut events = Vec::new();
    for packet in packets {
        for result in client.handle_input(&packet).expect("client input") {
            match result {
                ClientSessionResult::OutboundResponse(packet) => outbound.push_back(packet.bytes),
                ClientSessionResult::RaisedEvent(event) => events.push(event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
    }
    (outbound, events)
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

fn event_labels(events: &[ClientSessionEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            ClientSessionEvent::StreamMetadataReceived { .. } => "metadata",
            ClientSessionEvent::AudioDataReceived { .. } => "audio",
            ClientSessionEvent::VideoDataReceived { .. } => "video",
            _ => "status",
        })
        .collect()
}

fn has_status(events: &[ClientSessionEvent], expected: &str) -> bool {
    events.iter().any(|event| match event {
        ClientSessionEvent::UnhandleableOnStatusCode { code } => code == expected,
        ClientSessionEvent::UnknownTransactionResultReceived {
            additional_values,
            ..
        } => additional_values.iter().any(|value| {
            matches!(
                value,
                Amf0Value::Object(properties)
                    if matches!(properties.get("code"), Some(Amf0Value::Utf8String(code)) if code == expected)
            )
        }),
        _ => false,
    })
}

fn test_flv() -> Vec<u8> {
    let mut bytes = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00".to_vec();
    append_flv_tag(&mut bytes, 8, 0, &[0xaf, 0x00, 0x12]);
    append_flv_tag(&mut bytes, 9, 10, &[0x17, 0x01, 0x00, 0x00, 0x00, 0xaa]);
    bytes
}

fn append_flv_tag(bytes: &mut Vec<u8>, tag_type: u8, timestamp: u32, payload: &[u8]) {
    let size = u32::try_from(payload.len()).expect("test FLV payload size");
    let size_bytes = size.to_be_bytes();
    let timestamp_bytes = timestamp.to_be_bytes();
    bytes.push(tag_type);
    bytes.extend_from_slice(&[
        size_bytes[1],
        size_bytes[2],
        size_bytes[3],
        timestamp_bytes[1],
        timestamp_bytes[2],
        timestamp_bytes[3],
        timestamp_bytes[0],
        0,
        0,
        0,
    ]);
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&(size + 11).to_be_bytes());
}
