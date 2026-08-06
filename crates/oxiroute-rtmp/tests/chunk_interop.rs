use bytes::Bytes;
use rml_rtmp::{
    chunk_io::{ChunkDeserializationError, ChunkDeserializer, ChunkSerializer},
    messages::MessagePayload,
    time::RtmpTimestamp,
};

#[test]
fn fragmented_input_round_trips_header_formats_zero_through_three() {
    let messages = vec![
        payload(100, 50, vec![1, 2, 3, 4]),
        payload(110, 50, vec![5, 6, 7, 8]),
        payload(120, 50, vec![9, 10, 11, 12]),
        payload(130, 51, vec![13, 14, 15, 16, 17]),
    ];
    let mut serializer = ChunkSerializer::new();
    let mut wire = Vec::new();
    let mut formats = Vec::new();
    for message in &messages {
        let packet = serializer
            .serialize(message, false, false)
            .expect("serialize RTMP message");
        formats.push(packet.bytes[0] >> 6);
        wire.extend_from_slice(&packet.bytes);
    }

    assert_eq!(formats, [0, 2, 3, 1]);
    assert_eq!(
        decode_one_byte_at_a_time(&wire).expect("decode fragmented input"),
        messages
    );
}

#[test]
fn fragmented_payload_and_extended_timestamp_round_trip() {
    let fragmented = payload(1_000, 50, (0..=255).collect());
    let extended = payload(16_777_216, 51, vec![0xaa, 0xbb, 0xcc]);
    let mut serializer = ChunkSerializer::new();
    let fragmented_packet = serializer
        .serialize(&fragmented, false, false)
        .expect("serialize fragmented message");
    let extended_packet = serializer
        .serialize(&extended, true, false)
        .expect("serialize extended timestamp message");

    let continuation_offset = 1 + 3 + 3 + 1 + 4 + 128;
    assert_eq!(fragmented_packet.bytes[continuation_offset] >> 6, 3);
    assert_eq!(&extended_packet.bytes[1..4], &[0xff, 0xff, 0xff]);
    assert_eq!(
        &extended_packet.bytes[12..16],
        &16_777_216_u32.to_be_bytes()
    );

    let mut wire = fragmented_packet.bytes;
    wire.extend_from_slice(&extended_packet.bytes);
    assert_eq!(
        decode_one_byte_at_a_time(&wire).expect("decode fragmented and extended input"),
        vec![fragmented, extended]
    );
}

fn payload(timestamp: u32, type_id: u8, data: Vec<u8>) -> MessagePayload {
    MessagePayload {
        timestamp: RtmpTimestamp::new(timestamp),
        message_stream_id: 12,
        type_id,
        data: Bytes::from(data),
    }
}

fn decode_one_byte_at_a_time(
    wire: &[u8],
) -> Result<Vec<MessagePayload>, ChunkDeserializationError> {
    let mut deserializer = ChunkDeserializer::new();
    let mut messages = Vec::new();
    for byte in wire {
        let input = [*byte];
        let mut input = input.as_slice();
        loop {
            let Some(message) = deserializer.get_next_message(input)? else {
                break;
            };
            messages.push(message);
            input = &[];
        }
    }
    while let Some(message) = deserializer.get_next_message(&[])? {
        messages.push(message);
    }
    Ok(messages)
}
