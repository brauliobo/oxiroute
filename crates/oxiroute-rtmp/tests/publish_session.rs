use std::{collections::VecDeque, sync::Arc};

use bytes::Bytes;
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MAX_RTMP_QUERY_BYTES, MAX_RTMP_STREAM_NAME_BYTES, RtmpApplication,
    RtmpCapabilities, RtmpRegistry, RtmpSession, RtmpSessionPolicy, StreamKey,
    VideoCodecIdentifier,
};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
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
fn rejects_a_second_publisher_for_the_same_stream() {
    let registry = registry();
    let hub = LiveHub::new(LiveHubLimits::default());
    let mut first_server = session(Arc::clone(&registry), hub.clone());
    let mut second_server = session(Arc::clone(&registry), hub);
    let mut first_client = connect(&mut first_server, "broadcast");
    let mut second_client = connect(&mut second_server, "broadcast");

    let first_events = request_publish(&mut first_client, &mut first_server, 2_100);
    let second_events = request_publish(&mut second_client, &mut second_server, 2_200);

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

    first_server.close(2_300).expect("detach first publisher");
    second_server
        .close(2_300)
        .expect("close rejected publisher session");
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
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
    );
    client
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
