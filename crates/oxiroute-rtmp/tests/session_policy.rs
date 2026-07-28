use std::sync::Arc;

use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, MAX_INBOUND_CHUNK_SIZE, MAX_INBOUND_MESSAGE_SIZE, RtmpApplication,
    RtmpCapabilities, RtmpRegistry, RtmpServiceRuntime, RtmpSession, RtmpSessionPolicy,
};
use rml_rtmp::{
    chunk_io::ChunkDeserializer,
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    messages::RtmpMessage,
};

#[test]
fn configured_outbound_chunk_size_is_announced_on_the_wire() {
    let runtime = RtmpServiceRuntime::new(
        "edge",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::with_outbound_chunk_size(
            [RtmpApplication::new("live", true, true)],
            8_192,
        ),
    );
    let mut server = runtime.session();
    let mut client_handshake = Handshake::new(PeerType::Client);
    let hello = client_handshake
        .generate_outbound_p0_and_p1()
        .expect("client hello");
    let server_hello = server.receive(&hello, 1).expect("server hello");
    let finish = match client_handshake
        .process_bytes(&server_hello.concat())
        .expect("server handshake response")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => panic!("incomplete client handshake"),
    };
    let startup = server.receive(&finish, 2).expect("server startup packets");
    let mut deserializer = ChunkDeserializer::new();
    let mut announced = None;
    for packet in startup {
        let mut input = packet.as_slice();
        while let Some(payload) = deserializer
            .get_next_message(input)
            .expect("decode server startup packet")
        {
            input = &[];
            if let RtmpMessage::SetChunkSize { size } = payload
                .to_rtmp_message()
                .expect("decode server startup message")
            {
                announced = Some(size);
                deserializer
                    .set_max_chunk_size(size as usize)
                    .expect("apply announced chunk size");
            }
        }
    }
    assert_eq!(announced, Some(8_192));
}

#[test]
fn inbound_wire_rejects_zero_and_over_limit_chunk_sizes_before_they_take_effect() {
    for size in [0, MAX_INBOUND_CHUNK_SIZE + 1] {
        let mut server = connected_server();
        let mut chunk = vec![0x02, 0, 0, 0, 0, 0, 4, 1, 0, 0, 0, 0];
        chunk.extend_from_slice(&size.to_be_bytes());

        let error = server.receive(&chunk, 3).expect_err("invalid chunk size");

        assert!(error.to_string().contains("chunk size"), "{error}");
    }
}

#[test]
fn inbound_wire_rejects_zero_and_over_limit_message_lengths_before_payload_allocation() {
    for length in [0, MAX_INBOUND_MESSAGE_SIZE + 1] {
        let mut server = connected_server();
        let length = u32::try_from(length).expect("wire message length");
        let chunk = vec![
            0x03,
            0,
            0,
            0,
            ((length >> 16) & 0xff) as u8,
            ((length >> 8) & 0xff) as u8,
            (length & 0xff) as u8,
            9,
            1,
            0,
            0,
            0,
        ];

        let error = server
            .receive(&chunk, 3)
            .expect_err("invalid message length");

        assert!(error.to_string().contains("message length"), "{error}");
    }
}

fn connected_server() -> RtmpSession {
    let runtime = RtmpServiceRuntime::new(
        "edge",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::with_outbound_chunk_size(
            [RtmpApplication::new("live", true, true)],
            8_192,
        ),
    );
    let mut server = runtime.session();
    let mut client_handshake = Handshake::new(PeerType::Client);
    let hello = client_handshake
        .generate_outbound_p0_and_p1()
        .expect("client hello");
    let server_hello = server.receive(&hello, 1).expect("server hello");
    let finish = match client_handshake
        .process_bytes(&server_hello.concat())
        .expect("server handshake response")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => panic!("incomplete client handshake"),
    };
    server.receive(&finish, 2).expect("server startup packets");
    server
}
