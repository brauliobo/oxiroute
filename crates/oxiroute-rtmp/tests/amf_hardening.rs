use std::{panic::AssertUnwindSafe, sync::Arc};

use bytes::Bytes;
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, RtmpApplication, RtmpCapabilities, RtmpRegistry, RtmpServiceRuntime,
    RtmpSession, RtmpSessionLimits, RtmpSessionPolicy,
};
use rml_rtmp::{
    chunk_io::ChunkSerializer,
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    messages::{Amf0Limits, MessagePayload, RtmpMessage},
    time::RtmpTimestamp,
};

#[test]
fn rejects_amf0_container_and_nesting_bombs() {
    for data in [object_with_entries(1_025), strict_array_with_values(1_025)] {
        assert!(decode_amf0(data).is_err());
    }

    assert!(decode_amf0(nested_objects(40)).is_err());
}

#[test]
fn rejects_trailing_amf0_object_end_marker() {
    assert!(decode_amf0(vec![0x05, 0x09]).is_err());
}

#[test]
fn rejects_amf0_value_count_bomb() {
    let mut data = Vec::new();
    data.extend(std::iter::repeat_n(0x05, 4_097));

    assert!(decode_amf0(data).is_err());
}

#[test]
fn rejects_declared_amf0_bounds_before_reading_or_allocating_payloads() {
    let limits = Amf0Limits::new(4, 4, 8, 4);

    let mut ecma_array = vec![0x08];
    ecma_array.extend_from_slice(&u32::MAX.to_be_bytes());
    ecma_array.extend_from_slice(&[0, 0, 0x09]);
    assert!(decode_amf0_with_limits(ecma_array, &limits).is_err());

    let string = vec![0x02, 0, 5, b'a', b'b', b'c', b'd', b'e'];
    assert!(decode_amf0_with_limits(string, &limits).is_err());
}

#[test]
fn session_adapter_applies_custom_chunk_message_and_amf0_limits() {
    let limits = RtmpSessionLimits::new(64, 16, 4, 4, 8, 8);
    let mut server = connected_server(limits);
    let mut serializer = ChunkSerializer::new();
    let chunk_size = RtmpMessage::SetChunkSize { size: 65 }
        .into_message_payload(RtmpTimestamp::new(0), 0)
        .expect("chunk size payload");
    let chunk_size_packet = serializer
        .serialize(&chunk_size, true, false)
        .expect("chunk size packet");
    let error = server
        .receive(&chunk_size_packet.bytes, 3)
        .expect_err("oversized chunk size");
    assert!(error.to_string().contains("chunk size"), "{error}");

    let mut server = connected_server(limits);
    let oversized_message = MessagePayload {
        timestamp: RtmpTimestamp::new(0),
        type_id: 9,
        message_stream_id: 0,
        data: Bytes::from(vec![0; 17]),
    };
    let packet = serializer
        .serialize(&oversized_message, true, false)
        .expect("oversized message packet");
    let error = server
        .receive(&packet.bytes, 3)
        .expect_err("oversized message");
    assert!(error.to_string().contains("message length"), "{error}");

    let mut server = connected_server(limits);
    let amf0_bomb = MessagePayload {
        timestamp: RtmpTimestamp::new(0),
        type_id: 18,
        message_stream_id: 0,
        data: Bytes::from(strict_array_with_values(5)),
    };
    let packet = serializer
        .serialize(&amf0_bomb, true, false)
        .expect("AMF0 packet");
    assert!(server.receive(&packet.bytes, 3).is_err());
}

#[test]
fn malformed_amf0_inputs_do_not_panic() {
    for seed in 0_u32..256 {
        let mut input = Vec::with_capacity((seed as usize % 257) + 1);
        let mut state = seed.wrapping_mul(0x9e37_79b9).wrapping_add(1);
        for _ in 0..input.capacity() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            input.push(state.to_le_bytes()[0]);
        }

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = decode_amf0(input.clone());
        }));
        assert!(result.is_ok(), "AMF input {seed} panicked");
    }
}

#[test]
fn malformed_amf0_command_inputs_do_not_panic() {
    for type_id in [17, 20] {
        let payload = MessagePayload {
            timestamp: RtmpTimestamp::new(0),
            type_id,
            message_stream_id: 0,
            data: Bytes::new(),
        };
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| payload.to_rtmp_message()));

        assert!(
            matches!(result, Ok(Err(_))),
            "AMF command type {type_id} panicked"
        );
    }
}

fn decode_amf0(
    data: Vec<u8>,
) -> Result<rml_rtmp::messages::RtmpMessage, rml_rtmp::messages::MessageDeserializationError> {
    let payload = MessagePayload {
        timestamp: RtmpTimestamp::new(0),
        type_id: 18,
        message_stream_id: 0,
        data: Bytes::from(data),
    };
    payload.to_rtmp_message()
}

fn decode_amf0_with_limits(
    data: Vec<u8>,
    limits: &Amf0Limits,
) -> Result<rml_rtmp::messages::RtmpMessage, rml_rtmp::messages::MessageDeserializationError> {
    let payload = MessagePayload {
        timestamp: RtmpTimestamp::new(0),
        type_id: 18,
        message_stream_id: 0,
        data: Bytes::from(data),
    };
    payload.to_rtmp_message_with_limits(limits)
}

fn connected_server(limits: RtmpSessionLimits) -> RtmpSession {
    let runtime = RtmpServiceRuntime::new(
        "edge",
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::with_inbound_limits([RtmpApplication::new("live", true, true)], limits),
    );
    let mut server = runtime.session();
    let mut client = Handshake::new(PeerType::Client);
    let hello = client.generate_outbound_p0_and_p1().expect("client hello");
    let server_hello = server.receive(&hello, 1).expect("server hello");
    let finish = match client
        .process_bytes(&server_hello.concat())
        .expect("client handshake response")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => panic!("incomplete client handshake"),
    };
    server.receive(&finish, 2).expect("server startup");
    server
}

fn object_with_entries(entries: usize) -> Vec<u8> {
    let mut data = vec![0x03];
    for index in 0..entries {
        data.extend_from_slice(&1_u16.to_be_bytes());
        data.push(b'a' + u8::try_from(index % 26).expect("bounded property name"));
        data.push(0x05);
    }
    data.extend_from_slice(&[0, 0, 0x09]);
    data
}

fn strict_array_with_values(values: usize) -> Vec<u8> {
    let mut data = vec![0x0a];
    data.extend_from_slice(&u32::try_from(values).expect("bounded array").to_be_bytes());
    data.extend(std::iter::repeat_n(0x05, values));
    data
}

fn nested_objects(depth: usize) -> Vec<u8> {
    let mut value = vec![0x05];
    for _ in 0..depth {
        let mut object = vec![0x03, 0, 1, b'x'];
        object.extend_from_slice(&value);
        object.extend_from_slice(&[0, 0, 0x09]);
        value = object;
    }
    value
}
