use std::{collections::VecDeque, net::IpAddr, sync::Arc};

use bytes::Bytes;
use oxiroute_rtmp::{
    CatalogError, LiveHub, LiveHubError, LiveHubLimits, MAX_RTMP_QUERY_BYTES,
    MAX_RTMP_STREAM_NAME_BYTES, MediaSnapshot, RTMP_STALE_PUBLISHER_THRESHOLD_MS, RtmpAccessAction,
    RtmpAccessPlan, RtmpAccessPolicy, RtmpAccessRulePlan, RtmpApplication, RtmpCapabilities,
    RtmpNetwork, RtmpRegistry, RtmpServiceRuntime, RtmpSession, RtmpSessionCeilings,
    RtmpSessionError, RtmpSessionPolicy, RtmpTokenPlan, StreamKey, VideoCodecIdentifier,
};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType, StreamMetadata,
    },
    time::RtmpTimestamp,
};

#[test]
fn publishes_media_into_the_catalog_and_detaches_on_stop() {
    let registry = registry();
    let mut server = session(
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
    );
    let mut client = connect(&mut server, "broadcast");

    let publish = client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("publish request");
    let events = exchange(&mut client, &mut server, vec![publish], 1_100);

    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams.len(), 1);
    assert_eq!(
        snapshot.streams[0].key,
        StreamKey::new("live", "broadcast", "camera")
    );
    assert_eq!(
        snapshot.streams[0]
            .publisher
            .expect("active publisher")
            .session_id,
        server.session_id()
    );

    let video = client
        .publish_video_data(
            Bytes::from_static(&[0x17, 0x01, 0x00, 0x00, 0x00, 0xaa]),
            RtmpTimestamp::new(42),
            false,
        )
        .expect("video packet");
    exchange(&mut client, &mut server, vec![video], 1_200);

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams[0].media.video.flv_codec_id, Some(7));
    assert_eq!(snapshot.streams[0].media.video.payload_bytes_received, 6);
    assert_eq!(
        snapshot.streams[0].media.video.last_rtmp_timestamp_ms,
        Some(42)
    );
    assert_eq!(
        snapshot.streams[0].media.video.last_observed_at_unix_ms,
        Some(1_200)
    );

    let stop = client.stop_publishing().expect("stop publishing");
    exchange(&mut client, &mut server, stop, 1_300);

    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn publisher_catalog_snapshot_uses_one_observation_for_each_media_event() {
    let registry = registry();
    let mut server = session(
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
    );
    let mut client = connect(&mut server, "broadcast");
    request_publish(&mut client, &mut server, 2_000);

    let mut metadata = StreamMetadata::new();
    metadata.audio_codec_id = Some(10);
    metadata.video_codec_id = Some(7);
    let metadata = client.publish_metadata(&metadata).expect("metadata packet");
    exchange(&mut client, &mut server, vec![metadata], 2_100);
    let audio = client
        .publish_audio_data(
            Bytes::from_static(&[0xaf, 0x01, 0x44]),
            RtmpTimestamp::new(42),
            false,
        )
        .expect("audio packet");
    exchange(&mut client, &mut server, vec![audio], 2_200);
    let video = client
        .publish_video_data(
            Bytes::from_static(&[0x91, b'h', b'v', b'c', b'1', 0x55]),
            RtmpTimestamp::new(40),
            false,
        )
        .expect("video packet");
    exchange(&mut client, &mut server, vec![video], 2_300);

    let media = registry.snapshot().streams[0].media;
    assert_eq!(media.audio.flv_codec_id, Some(10));
    assert_eq!(media.audio.payload_bytes_received, 3);
    assert_eq!(media.audio.last_rtmp_timestamp_ms, Some(42));
    assert_eq!(media.audio.last_observed_at_unix_ms, Some(2_200));
    assert_eq!(
        media.video.video_codec,
        Some(VideoCodecIdentifier::FourCc(*b"hvc1"))
    );
    assert_eq!(media.video.flv_codec_id, None);
    assert_eq!(media.video.payload_bytes_received, 6);
    assert_eq!(media.video.last_rtmp_timestamp_ms, Some(40));
    assert_eq!(media.video.last_observed_at_unix_ms, Some(2_300));
    assert_eq!(media.fanout_payload_bytes_queued, 0);
}

#[test]
fn rejects_a_second_publisher_for_the_same_stream() {
    let registry = registry();
    let hub = LiveHub::new(LiveHubLimits::default());
    let mut first_server = session(Arc::clone(&registry), hub.clone());
    let mut second_server = session(Arc::clone(&registry), hub);
    let mut first_client = connect(&mut first_server, "broadcast");
    let mut second_client = connect(&mut second_server, "broadcast");
    let attached_at = 2_100;
    let media_at = attached_at + 100;

    let first_events = request_publish(&mut first_client, &mut first_server, attached_at);
    let first_media = first_client
        .publish_audio_data(
            Bytes::from_static(&[0xaf, 0x01, 0x44]),
            RtmpTimestamp::new(1),
            false,
        )
        .expect("first audio packet");
    exchange(
        &mut first_client,
        &mut first_server,
        vec![first_media],
        media_at,
    );
    let fresh_duplicate_at = media_at + RTMP_STALE_PUBLISHER_THRESHOLD_MS - 1;
    let second_events = request_publish(&mut second_client, &mut second_server, fresh_duplicate_at);

    assert!(
        first_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
    assert!(
        !second_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams.len(), 1);
    assert_eq!(
        snapshot.streams[0]
            .publisher
            .expect("first publisher remains active")
            .session_id,
        first_server.session_id()
    );

    first_server
        .close(fresh_duplicate_at + 1)
        .expect("detach first publisher");
    second_server
        .close(fresh_duplicate_at + 1)
        .expect("close rejected publisher session");
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn stale_publisher_takeover_expires_the_old_role_without_clearing_the_new_owner() {
    let registry = registry();
    let hub = LiveHub::new(LiveHubLimits::default());
    let mut first_server = session(Arc::clone(&registry), hub.clone());
    let mut second_server = session(Arc::clone(&registry), hub.clone());
    let mut first_client = connect(&mut first_server, "broadcast");
    let mut second_client = connect(&mut second_server, "broadcast");
    let attached_at = 2_100;
    let metadata_at = attached_at + 100;

    request_publish(&mut first_client, &mut first_server, attached_at);
    let first_session_id = first_server.session_id();
    let mut metadata = StreamMetadata::new();
    metadata.encoder = Some("oxiroute-takeover-test".into());
    let first_metadata = first_client
        .publish_metadata(&metadata)
        .expect("first metadata packet");
    exchange(
        &mut first_client,
        &mut first_server,
        vec![first_metadata],
        metadata_at,
    );

    let takeover_at = metadata_at + RTMP_STALE_PUBLISHER_THRESHOLD_MS;
    assert!(!first_server.is_publisher_stale(takeover_at - 1));
    assert!(first_server.is_publisher_stale(takeover_at));
    let second_events = request_publish(&mut second_client, &mut second_server, takeover_at);
    assert!(
        second_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams.len(), 1);
    assert_eq!(
        snapshot.streams[0]
            .publisher
            .expect("replacement publisher")
            .session_id,
        second_server.session_id()
    );
    assert_eq!(snapshot.streams[0].media, MediaSnapshot::default());
    assert_eq!(hub.stats().publishers, 1);

    let stale_media = first_client
        .publish_audio_data(
            Bytes::from_static(&[0xaf, 0x01, 0x55]),
            RtmpTimestamp::new(2),
            false,
        )
        .expect("stale audio packet");
    let stale_packet = outbound_packets(vec![stale_media])
        .pop_front()
        .expect("stale audio wire packet");
    assert!(matches!(
        first_server.receive(&stale_packet, takeover_at + 1),
        Err(RtmpSessionError::LiveHub(
            LiveHubError::PublisherExpired { .. }
        ))
    ));
    assert_eq!(
        registry.snapshot().streams[0]
            .publisher
            .expect("replacement publisher remains")
            .session_id,
        second_server.session_id()
    );

    drop(first_server);
    assert_eq!(
        registry.snapshot().streams[0]
            .publisher
            .expect("replacement publisher survives stale cleanup")
            .session_id,
        second_server.session_id()
    );
    assert_ne!(first_session_id, second_server.session_id());
    second_server
        .close(takeover_at + 2)
        .expect("replacement close");
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn stale_publisher_close_cannot_clear_the_replacement_owner() {
    let registry = registry();
    let hub = LiveHub::new(LiveHubLimits::default());
    let mut first_server = session(Arc::clone(&registry), hub.clone());
    let mut second_server = session(Arc::clone(&registry), hub);
    let mut first_client = connect(&mut first_server, "broadcast");
    let mut second_client = connect(&mut second_server, "broadcast");
    let attached_at = 3_100;
    request_publish(&mut first_client, &mut first_server, attached_at);
    let second_events = request_publish(
        &mut second_client,
        &mut second_server,
        attached_at + RTMP_STALE_PUBLISHER_THRESHOLD_MS,
    );
    assert!(
        second_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );

    assert!(matches!(
        first_server.close(attached_at + RTMP_STALE_PUBLISHER_THRESHOLD_MS + 1),
        Err(CatalogError::PublisherMismatch { .. } | CatalogError::StreamNotFound(_))
    ));
    assert_eq!(
        registry.snapshot().streams[0]
            .publisher
            .expect("replacement publisher remains")
            .session_id,
        second_server.session_id()
    );
    second_server
        .close(attached_at + RTMP_STALE_PUBLISHER_THRESHOLD_MS + 2)
        .expect("replacement close");
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn treats_publish_query_arguments_as_non_identity_protocol_data() {
    let registry = registry();
    let hub = LiveHub::new(LiveHubLimits::default());
    let mut first_server = session(Arc::clone(&registry), hub.clone());
    let mut second_server = session(Arc::clone(&registry), hub);
    let mut first_client = connect(&mut first_server, "broadcast");
    let mut second_client = connect(&mut second_server, "broadcast");

    let first_events = request_named_publish(
        &mut first_client,
        &mut first_server,
        "camera?token=a",
        3_100,
    );
    let second_events = request_named_publish(
        &mut second_client,
        &mut second_server,
        "camera?token=b",
        3_200,
    );

    assert!(
        first_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
    assert!(
        !second_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
    let video = first_client
        .publish_video_data(
            Bytes::from_static(&[0x17, 0x01, 0x00, 0x00, 0x00, 0xaa]),
            RtmpTimestamp::new(77),
            false,
        )
        .expect("video packet");
    exchange(&mut first_client, &mut first_server, vec![video], 3_300);

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams.len(), 1);
    assert_eq!(
        snapshot.streams[0].key,
        StreamKey::new("live", "broadcast", "camera")
    );
    assert_eq!(snapshot.streams[0].media.video.payload_bytes_received, 6);

    first_server.close(3_400).expect("detach first publisher");
    second_server
        .close(3_400)
        .expect("close rejected publisher session");
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn enforces_ordered_publish_acl_and_stream_query_token() {
    let registry = registry();
    let publish = RtmpAccessPlan::new(
        [
            RtmpAccessRulePlan::new(
                RtmpAccessAction::Deny,
                RtmpNetwork::parse("192.0.2.0/24").expect("valid network"),
            )
            .expect("valid deny rule"),
            RtmpAccessRulePlan::new(RtmpAccessAction::Allow, RtmpNetwork::All)
                .expect("valid allow rule"),
        ],
        Some(RtmpTokenPlan::new("token", "secret").expect("valid token plan")),
    )
    .runtime_policy();
    let application = RtmpApplication::new("broadcast", true, true).with_authorization(
        publish,
        RtmpAccessPolicy::default(),
        RtmpSessionCeilings::default(),
    );
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([application]),
    );

    let mut denied_server = runtime.session_with_peer_addr(Some(
        "192.0.2.10".parse::<IpAddr>().expect("valid peer address"),
    ));
    let mut denied_client = connect(&mut denied_server, "broadcast");
    let denied_events = request_named_publish(
        &mut denied_client,
        &mut denied_server,
        "camera?token=secret",
        6_100,
    );
    assert!(
        !denied_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );

    let mut wrong_token_server = runtime.session_with_peer_addr(Some(
        "198.51.100.10"
            .parse::<IpAddr>()
            .expect("valid peer address"),
    ));
    let mut wrong_token_client = connect(&mut wrong_token_server, "broadcast");
    let wrong_token_events = request_named_publish(
        &mut wrong_token_client,
        &mut wrong_token_server,
        "camera?token=wrong",
        6_200,
    );
    assert!(
        !wrong_token_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );

    let mut accepted_server = runtime.session_with_peer_addr(Some(
        "198.51.100.10"
            .parse::<IpAddr>()
            .expect("valid peer address"),
    ));
    let mut accepted_client = connect(&mut accepted_server, "broadcast");
    let accepted_events = request_named_publish(
        &mut accepted_client,
        &mut accepted_server,
        "camera?token=secret",
        6_300,
    );
    assert!(
        accepted_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
}

#[test]
fn enforces_application_connection_ceiling_and_releases_it_on_close() {
    let registry = registry();
    let application = RtmpApplication::new("broadcast", true, true).with_authorization(
        RtmpAccessPolicy::default(),
        RtmpAccessPolicy::default(),
        RtmpSessionCeilings::new(1, 256, 1_024),
    );
    let runtime = RtmpServiceRuntime::new(
        "live",
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([application]),
    );

    let mut first_server = runtime.session();
    let (first_client, first_events) = connect_with_events(&mut first_server, "broadcast");
    assert!(
        first_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
    );

    let mut second_server = runtime.session();
    let (second_client, second_events) = connect_with_events(&mut second_server, "broadcast");
    assert!(second_events.iter().any(|event| matches!(
        event,
        ClientSessionEvent::ConnectionRequestRejected { description }
            if description == "RTMP application connection limit reached"
    )));
    drop(second_client);
    drop(second_server);

    drop(first_client);
    drop(first_server);
    let mut third_server = runtime.session();
    let (third_client, third_events) = connect_with_events(&mut third_server, "broadcast");
    assert!(
        third_events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
    );
    drop(third_client);
    drop(third_server);
}

#[test]
fn rejects_oversized_stream_and_query_identities_without_catalog_entries() {
    let registry = registry();
    let hub = LiveHub::new(LiveHubLimits::default());
    let mut server = session(Arc::clone(&registry), hub.clone());
    let mut client = connect(&mut server, "broadcast");

    let oversized_stream = "s".repeat(MAX_RTMP_STREAM_NAME_BYTES + 1);
    let events = request_named_publish(&mut client, &mut server, &oversized_stream, 4_100);
    assert!(has_status(&events, "NetStream.Publish.BadName"));
    assert!(registry.snapshot().streams.is_empty());
    assert_eq!(hub.stats().streams, 0);

    let mut server = session(Arc::clone(&registry), hub.clone());
    let mut client = connect(&mut server, "broadcast");
    let oversized_query = format!("camera?{}", "q".repeat(MAX_RTMP_QUERY_BYTES + 1));
    let events = request_named_publish(&mut client, &mut server, &oversized_query, 4_200);
    assert!(has_status(&events, "NetStream.Publish.BadName"));
    assert!(registry.snapshot().streams.is_empty());
    assert_eq!(hub.stats().streams, 0);
}

#[test]
fn publishes_enhanced_codec_identity_into_the_catalog() {
    let registry = registry();
    let mut server = session(
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
    );
    let mut client = connect(&mut server, "broadcast");
    request_publish(&mut client, &mut server, 5_100);

    let video = client
        .publish_video_data(
            Bytes::from_static(&[0x91, b'h', b'v', b'c', b'1', 0xaa]),
            RtmpTimestamp::new(42),
            false,
        )
        .expect("enhanced HEVC packet");
    exchange(&mut client, &mut server, vec![video], 5_200);

    let snapshot = registry.snapshot();
    let video = snapshot.streams[0].media.video;
    assert_eq!(video.flv_codec_id, None);
    assert_eq!(
        video.video_codec,
        Some(VideoCodecIdentifier::FourCc(*b"hvc1"))
    );
}

fn registry() -> Arc<RtmpRegistry> {
    Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }))
}

fn session(registry: Arc<RtmpRegistry>, hub: LiveHub) -> RtmpSession {
    RtmpSession::new(
        "live",
        registry,
        hub,
        RtmpSessionPolicy::new([RtmpApplication::new("broadcast", true, true)]),
    )
}

fn connect(server: &mut RtmpSession, application: &str) -> ClientSession {
    let (client, events) = connect_with_events(server, application);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
    );
    client
}

fn connect_with_events(
    server: &mut RtmpSession,
    application: &str,
) -> (ClientSession, Vec<ClientSessionEvent>) {
    let mut handshake = Handshake::new(PeerType::Client);
    let client_hello = handshake
        .generate_outbound_p0_and_p1()
        .expect("client hello");
    let server_hello = server.receive(&client_hello, 1_000).expect("server hello");
    let server_hello = server_hello.concat();
    let client_finish = match handshake
        .process_bytes(&server_hello)
        .expect("client handshake response")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        result @ HandshakeProcessResult::InProgress { .. } => {
            panic!("client handshake did not complete: {result:?}");
        }
    };
    let server_startup = server
        .receive(&client_finish, 1_000)
        .expect("server handshake completion");
    let (mut client, initial_results) =
        ClientSession::new(ClientSessionConfig::new()).expect("client session");
    assert!(initial_results.is_empty());
    assert!(
        feed_server_packets(&mut client, server_startup)
            .0
            .is_empty()
    );

    let request = client
        .request_connection(application.into())
        .expect("connection request");
    let events = exchange(&mut client, server, vec![request], 1_000);
    (client, events)
}

fn request_publish(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    request_named_publish(client, server, "camera", at_unix_ms)
}

fn request_named_publish(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    stream_name: &str,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    let request = client
        .request_publishing(stream_name.into(), PublishRequestType::Live)
        .expect("publish request");
    exchange(client, server, vec![request], at_unix_ms)
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

        let (next_client_packets, mut next_events) = feed_server_packets(client, server_packets);
        client_packets = next_client_packets;
        events.append(&mut next_events);
    }

    panic!("RTMP exchange did not settle");
}

fn feed_server_packets(
    client: &mut ClientSession,
    server_packets: Vec<Vec<u8>>,
) -> (VecDeque<Vec<u8>>, Vec<ClientSessionEvent>) {
    let mut client_packets = VecDeque::new();
    let mut events = Vec::new();

    for packet in server_packets {
        for result in client.handle_input(&packet).expect("client input") {
            match result {
                ClientSessionResult::OutboundResponse(packet) => {
                    client_packets.push_back(packet.bytes);
                }
                ClientSessionResult::RaisedEvent(event) => events.push(event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
    }

    (client_packets, events)
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

fn has_status(events: &[ClientSessionEvent], expected: &str) -> bool {
    events.iter().any(|event| match event {
        ClientSessionEvent::UnhandleableOnStatusCode { code } => code == expected,
        ClientSessionEvent::UnknownTransactionResultReceived {
            additional_values,
            ..
        } => additional_values.iter().any(|value| {
            matches!(
                value,
                rml_rtmp::rml_amf0::Amf0Value::Object(properties)
                    if matches!(properties.get("code"), Some(rml_rtmp::rml_amf0::Amf0Value::Utf8String(code)) if code == expected)
            )
        }),
        _ => false,
    })
}
