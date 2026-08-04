#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use rml_rtmp::{chunk_io::ChunkDeserializer, messages::RtmpMessage};

const MAX_INPUT_BYTES: usize = 256 * 1024;
const MAX_CHUNK_SIZE: usize = 64 * 1024;
const MAX_MESSAGE_SIZE: usize = 256 * 1024;
const MAX_MESSAGES_PER_INPUT: usize = 8;

fuzz_target!(|data: &[u8]| {
    let Some(mut data) =
        support::bounded_input(data, MAX_INPUT_BYTES).map(|data| data.into_owned())
    else {
        return;
    };
    if data == b"seed:chunk" {
        data = vec![
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x08, 0x01, 0x00, 0x00, 0x00, b'h', b'e',
            b'l', b'l', b'o',
        ];
    }

    let mut deserializer = ChunkDeserializer::new_with_limits(MAX_CHUNK_SIZE, MAX_MESSAGE_SIZE);
    let mut next = data.as_slice();
    for _ in 0..MAX_MESSAGES_PER_INPUT {
        match deserializer.get_next_message(next) {
            Ok(Some(payload)) => {
                next = &[];
                if let Ok(RtmpMessage::SetChunkSize { size }) = payload.to_rtmp_message() {
                    let size = usize::try_from(size)
                        .unwrap_or(MAX_CHUNK_SIZE)
                        .clamp(1, MAX_CHUNK_SIZE);
                    let _ = deserializer.set_max_chunk_size(size);
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
});
