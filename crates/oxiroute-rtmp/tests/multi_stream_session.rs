use std::{collections::HashMap, collections::HashSet, collections::VecDeque, sync::Arc};

use bytes::Bytes;
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MAX_RTMP_MESSAGE_STREAMS, RtmpApplication, RtmpCapabilities,
    RtmpRegistry, RtmpSession, RtmpSessionError, RtmpSessionPolicy,
};
use rml_rtmp::{
    chunk_io::{ChunkDeserializer, ChunkSerializer},
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    messages::RtmpMessage,
    rml_amf0::Amf0Value,
    sessions::{ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult},
    time::RtmpTimestamp,
};

#[test]
fn multiple_publishers_are_isolated_and_teardown_matches_the_message_stream() {
    let registry = registry();
    let mut session = connected_session(
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
    );

    session.create_stream(1, 1_000);
    session.publish(1, "camera-a", 1_001);
    session.create_stream(2, 1_002);
    session.publish(2, "camera-b", 1_003);

    let snapshot = session.server.client_snapshot().expect("session snapshot");
    assert_eq!(snapshot.message_streams.len(), 2);
    assert_eq!(
        snapshot
            .message_streams
            .iter()
            .map(|stream| stream.message_stream_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );

    session.metadata(1, 1_010);
    session.video(1, 1_011);
    session.video(2, 1_012);

    let catalog = registry.snapshot();
    assert_eq!(catalog.streams.len(), 2);
    let media_by_name = catalog
        .streams
        .iter()
        .map(|stream| {
            (
                stream.key.name.as_str(),
                stream.media.video.payload_bytes_received,
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(media_by_name.get("camera-a"), Some(&6));
    assert_eq!(media_by_name.get("camera-b"), Some(&6));

    let duplicate = session.publish_result(1, "camera-conflict", 1_013);
    assert!(
        duplicate
            .iter()
            .any(|(_, message)| is_status(message, "NetStream.Publish.BadName"))
    );
    assert_eq!(registry.snapshot().streams.len(), 2);

    session.delete(1, 1_014);
    let remaining = registry.snapshot();
    assert_eq!(remaining.streams.len(), 1);
    assert_eq!(remaining.streams[0].key.name, "camera-b");
    assert_eq!(
        session
            .server
            .client_snapshot()
            .expect("remaining session snapshot")
            .message_streams
            .iter()
            .map(|stream| stream.message_stream_id)
            .collect::<Vec<_>>(),
        [2]
    );

    session.delete(2, 1_015);
    assert!(registry.snapshot().streams.is_empty());
    assert!(
        session
            .server
            .client_snapshot()
            .expect("empty session snapshot")
            .message_streams
            .is_empty()
    );
}

#[test]
fn unknown_media_streams_fail_closed_and_stream_admission_is_bounded() {
    let unknown_registry = registry();
    let mut session = connected_session(
        Arc::clone(&unknown_registry),
        LiveHub::new(LiveHubLimits::default()),
    );

    session.create_stream(1, 2_000);
    session.publish(1, "camera", 2_001);
    let error = session
        .video_result(99, 2_002)
        .expect_err("unknown media stream must fail closed");
    assert!(matches!(
        error,
        RtmpSessionError::Session(
            rml_rtmp::sessions::ServerSessionError::InvalidMessageStream { stream_id: 99 }
        )
    ));

    let registry = registry();
    let mut session = connected_session(
        Arc::clone(&registry),
        LiveHub::new(LiveHubLimits::default()),
    );
    for stream_id in 1..=MAX_RTMP_MESSAGE_STREAMS {
        let stream_id = u32::try_from(stream_id).expect("message stream ID");
        session.create_stream(stream_id, 3_000 + u64::from(stream_id));
        session.publish(
            stream_id,
            &format!("camera-{stream_id}"),
            3_100 + u64::from(stream_id),
        );
    }
    assert_eq!(
        session
            .server
            .client_snapshot()
            .expect("bounded session snapshot")
            .message_streams
            .len(),
        MAX_RTMP_MESSAGE_STREAMS
    );

    let limit_messages = session.create_stream_result(9, 4_000);
    assert!(
        limit_messages
            .iter()
            .any(|(_, message)| is_status(message, "NetConnection.CreateStream.Failed"))
    );
    assert_eq!(registry.snapshot().streams.len(), MAX_RTMP_MESSAGE_STREAMS);

    for stream_id in 1..=u32::try_from(MAX_RTMP_MESSAGE_STREAMS).expect("stream bound") {
        session.delete(stream_id, 4_100 + u64::from(stream_id));
    }
    assert!(registry.snapshot().streams.is_empty());
}

#[test]
fn multiple_playback_streams_retain_message_stream_ids_and_fanout() {
    let registry = registry();
    let hub = LiveHub::new(LiveHubLimits::default());
    let mut publisher = connected_session(Arc::clone(&registry), hub.clone());
    publisher.create_stream(1, 5_000);
    publisher.publish(1, "camera", 5_001);
    publisher.metadata(1, 5_002);

    let mut viewer = connected_session(Arc::clone(&registry), hub.clone());
    viewer.create_stream(1, 5_100);
    viewer.play(1, "camera", 5_101);
    let duplicate_play = viewer.command(
        "play",
        1,
        0,
        vec![Amf0Value::Utf8String("other-camera".into())],
        5_102,
    );
    assert!(
        duplicate_play
            .iter()
            .any(|(_, message)| is_status(message, "NetStream.Play.Failed"))
    );
    viewer.create_stream(2, 5_102);
    viewer.play(2, "camera", 5_103);

    let snapshot = viewer.server.client_snapshot().expect("viewer snapshot");
    assert_eq!(snapshot.message_streams.len(), 2);
    assert!(
        snapshot
            .message_streams
            .iter()
            .all(|stream| stream.role == oxiroute_rtmp::RtmpSessionRole::Subscriber)
    );
    assert_eq!(registry.snapshot().streams[0].subscriber_count, 2);
    assert_eq!(hub.stats().subscribers, 2);

    publisher.video(1, 5_110);
    let messages = viewer.drain_playback(32);
    let video_streams = messages
        .iter()
        .filter_map(|(stream_id, message)| match message {
            RtmpMessage::VideoData { .. } => Some(*stream_id),
            _ => None,
        })
        .collect::<HashSet<_>>();
    assert_eq!(video_streams, HashSet::from([1, 2]));

    viewer.delete(1, 5_120);
    assert_eq!(registry.snapshot().streams[0].subscriber_count, 1);
    assert_eq!(hub.stats().subscribers, 1);
    assert_eq!(
        viewer
            .server
            .client_snapshot()
            .expect("single viewer snapshot")
            .message_streams
            .iter()
            .map(|stream| stream.message_stream_id)
            .collect::<Vec<_>>(),
        [2]
    );
    viewer.delete(2, 5_121);
    assert_eq!(registry.snapshot().streams[0].subscriber_count, 0);
    assert_eq!(hub.stats().subscribers, 0);

    publisher.delete(1, 5_122);
    assert!(registry.snapshot().streams.is_empty());
}

fn registry() -> Arc<RtmpRegistry> {
    Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }))
}

struct WireSession {
    server: RtmpSession,
    command_serializer: ChunkSerializer,
    outbound_deserializer: ChunkDeserializer,
}

impl WireSession {
    fn create_stream(&mut self, stream_id: u32, at_unix_ms: u64) {
        let messages = self.create_stream_result(stream_id, at_unix_ms);
        assert!(
            messages
                .iter()
                .any(|(_, message)| is_command(message, "_result"))
        );
    }

    fn create_stream_result(
        &mut self,
        _expected_stream_id: u32,
        at_unix_ms: u64,
    ) -> Vec<(u32, RtmpMessage)> {
        self.command("createStream", 0, 0, Vec::new(), at_unix_ms)
    }

    fn publish(&mut self, stream_id: u32, stream_name: &str, at_unix_ms: u64) {
        let messages = self.publish_result(stream_id, stream_name, at_unix_ms);
        assert!(
            messages
                .iter()
                .any(|(_, message)| is_status(message, "NetStream.Publish.Start"))
        );
    }

    fn publish_result(
        &mut self,
        stream_id: u32,
        stream_name: &str,
        at_unix_ms: u64,
    ) -> Vec<(u32, RtmpMessage)> {
        self.command(
            "publish",
            stream_id,
            0,
            vec![
                Amf0Value::Utf8String(stream_name.to_owned()),
                Amf0Value::Utf8String("live".to_owned()),
            ],
            at_unix_ms,
        )
    }

    fn play(&mut self, stream_id: u32, stream_name: &str, at_unix_ms: u64) {
        let messages = self.command(
            "play",
            stream_id,
            0,
            vec![Amf0Value::Utf8String(stream_name.to_owned())],
            at_unix_ms,
        );
        assert!(
            messages
                .iter()
                .any(|(_, message)| is_status(message, "NetStream.Play.Start"))
        );
    }

    fn metadata(&mut self, stream_id: u32, at_unix_ms: u64) {
        let mut properties = HashMap::new();
        properties.insert("videocodecid".to_owned(), Amf0Value::Number(7.0));
        properties.insert("audiocodecid".to_owned(), Amf0Value::Number(10.0));
        let message = RtmpMessage::Amf0Data {
            values: vec![
                Amf0Value::Utf8String("@setDataFrame".to_owned()),
                Amf0Value::Utf8String("onMetaData".to_owned()),
                Amf0Value::Object(properties),
            ],
        };
        self.media(stream_id, message, at_unix_ms);
    }

    fn video(&mut self, stream_id: u32, at_unix_ms: u64) {
        self.video_result(stream_id, at_unix_ms)
            .expect("video packet");
    }

    fn video_result(&mut self, stream_id: u32, at_unix_ms: u64) -> Result<(), RtmpSessionError> {
        let message = RtmpMessage::VideoData {
            data: Bytes::from_static(&[0x17, 0x01, 0x00, 0x00, 0x00, 0x01]),
        };
        self.media_result(stream_id, message, at_unix_ms)
            .map(|_| ())
    }

    fn delete(&mut self, stream_id: u32, at_unix_ms: u64) {
        self.command(
            "deleteStream",
            stream_id,
            0,
            vec![Amf0Value::Number(f64::from(stream_id))],
            at_unix_ms,
        );
    }

    fn drain_playback(&mut self, maximum_events: usize) -> Vec<(u32, RtmpMessage)> {
        let packets = self
            .server
            .drain_playback(maximum_events)
            .expect("playback drain");
        decode_packets(&mut self.outbound_deserializer, packets)
    }

    fn command(
        &mut self,
        command_name: &str,
        message_stream_id: u32,
        transaction_id: u32,
        additional_arguments: Vec<Amf0Value>,
        at_unix_ms: u64,
    ) -> Vec<(u32, RtmpMessage)> {
        let message = RtmpMessage::Amf0Command {
            command_name: command_name.to_owned(),
            transaction_id: f64::from(transaction_id),
            command_object: Amf0Value::Null,
            additional_arguments,
        };
        let payload = message
            .into_message_payload(RtmpTimestamp::new(0), message_stream_id)
            .expect("command payload");
        let packet = self
            .command_serializer
            .serialize(&payload, false, false)
            .expect("command packet");
        let responses = self
            .server
            .receive(&packet.bytes, at_unix_ms)
            .expect("server command");
        decode_packets(&mut self.outbound_deserializer, responses)
    }

    fn media(&mut self, stream_id: u32, message: RtmpMessage, at_unix_ms: u64) {
        self.media_result(stream_id, message, at_unix_ms)
            .expect("media packet");
    }

    fn media_result(
        &mut self,
        stream_id: u32,
        message: RtmpMessage,
        at_unix_ms: u64,
    ) -> Result<Vec<(u32, RtmpMessage)>, RtmpSessionError> {
        let payload = message
            .into_message_payload(RtmpTimestamp::new(0), stream_id)
            .expect("media payload");
        let packet = self
            .command_serializer
            .serialize(&payload, false, false)
            .expect("media packet");
        let responses = self.server.receive(&packet.bytes, at_unix_ms)?;
        Ok(decode_packets(&mut self.outbound_deserializer, responses))
    }
}

fn connected_session(registry: Arc<RtmpRegistry>, hub: LiveHub) -> WireSession {
    let mut server = RtmpSession::new(
        "live",
        registry,
        hub,
        RtmpSessionPolicy::new([RtmpApplication::new("broadcast", true, true)]),
    );
    let mut handshake = Handshake::new(PeerType::Client);
    let hello = handshake
        .generate_outbound_p0_and_p1()
        .expect("client hello");
    let server_hello = server.receive(&hello, 1).expect("server hello");
    let finish = match handshake
        .process_bytes(&server_hello.concat())
        .expect("server handshake response")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => panic!("incomplete client handshake"),
    };
    let startup = server.receive(&finish, 2).expect("server startup packets");
    let (mut client, initial) = ClientSession::new(ClientSessionConfig::new()).expect("client");
    assert!(initial.is_empty());
    let mut outbound_deserializer = ChunkDeserializer::new();
    decode_packets(&mut outbound_deserializer, startup.clone());
    let mut client_packets = VecDeque::new();
    for packet in startup {
        for result in client.handle_input(&packet).expect("client startup") {
            if let ClientSessionResult::OutboundResponse(packet) = result {
                client_packets.push_back(packet.bytes);
            }
        }
    }
    let request = client
        .request_connection("broadcast".to_owned())
        .expect("connection request");
    if let ClientSessionResult::OutboundResponse(packet) = request {
        client_packets.push_back(packet.bytes);
    }
    let mut connected = false;
    for _ in 0..8 {
        if client_packets.is_empty() {
            break;
        }
        let mut next = VecDeque::new();
        while let Some(packet) = client_packets.pop_front() {
            let responses = server.receive(&packet, 3).expect("server connection input");
            for response in responses {
                for result in client
                    .handle_input(&response)
                    .expect("client connection response")
                {
                    match result {
                        ClientSessionResult::OutboundResponse(packet) => {
                            next.push_back(packet.bytes);
                        }
                        ClientSessionResult::RaisedEvent(
                            ClientSessionEvent::ConnectionRequestAccepted,
                        ) => {
                            connected = true;
                        }
                        ClientSessionResult::RaisedEvent(_)
                        | ClientSessionResult::UnhandleableMessageReceived(_) => {}
                    }
                }
                decode_packets(&mut outbound_deserializer, vec![response]);
            }
        }
        client_packets = next;
    }
    assert!(connected, "connection request was not accepted");
    WireSession {
        server,
        command_serializer: ChunkSerializer::new(),
        outbound_deserializer,
    }
}

fn decode_packets(
    deserializer: &mut ChunkDeserializer,
    packets: Vec<Vec<u8>>,
) -> Vec<(u32, RtmpMessage)> {
    let mut messages = Vec::new();
    for packet in packets {
        let mut bytes = packet.as_slice();
        while let Some(payload) = deserializer
            .get_next_message(bytes)
            .expect("decode server packet")
        {
            bytes = &[];
            let message_stream_id = payload.message_stream_id;
            let message = payload.to_rtmp_message().expect("decode server message");
            if let RtmpMessage::SetChunkSize { size } = &message {
                deserializer
                    .set_max_chunk_size(*size as usize)
                    .expect("apply server chunk size");
            }
            messages.push((message_stream_id, message));
        }
    }
    messages
}

fn is_command(message: &RtmpMessage, expected_name: &str) -> bool {
    matches!(
        message,
        RtmpMessage::Amf0Command { command_name, .. } if command_name == expected_name
    )
}

fn is_status(message: &RtmpMessage, expected_code: &str) -> bool {
    let RtmpMessage::Amf0Command {
        command_name,
        additional_arguments,
        ..
    } = message
    else {
        return false;
    };
    if command_name != "onStatus" && command_name != "_error" {
        return false;
    }
    matches!(
        additional_arguments.first(),
        Some(Amf0Value::Object(properties))
            if matches!(properties.get("code"), Some(Amf0Value::Utf8String(code)) if code == expected_code)
    )
}
