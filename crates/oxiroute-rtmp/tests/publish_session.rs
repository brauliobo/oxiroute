use std::{collections::VecDeque, sync::Arc};

use bytes::Bytes;
use oxiroute_rtmp::{RtmpCapabilities, RtmpPublishSession, RtmpRegistry, StreamKey};
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
    let mut server = RtmpPublishSession::new("live", Arc::clone(&registry));
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
            Bytes::from_static(&[0x17, 0x01, 0x00, 0x00, 0x00]),
            RtmpTimestamp::new(42),
            false,
        )
        .expect("video packet");
    exchange(&mut client, &mut server, vec![video], 1_200);

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams[0].media.video.flv_codec_id, Some(7));
    assert_eq!(snapshot.streams[0].media.video.payload_bytes_received, 5);
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
    let mut first_server = RtmpPublishSession::new("live", Arc::clone(&registry));
    let mut second_server = RtmpPublishSession::new("live", Arc::clone(&registry));
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

fn registry() -> Arc<RtmpRegistry> {
    Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }))
}

fn connect(server: &mut RtmpPublishSession, application: &str) -> ClientSession {
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
    server: &mut RtmpPublishSession,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    let request = client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("publish request");
    exchange(client, server, vec![request], at_unix_ms)
}

fn exchange(
    client: &mut ClientSession,
    server: &mut RtmpPublishSession,
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
