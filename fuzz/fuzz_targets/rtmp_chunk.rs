#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use rml_rtmp::{chunk_io::ChunkDeserializer, messages::RtmpMessage};

const MAX_INPUT_BYTES: usize = 262_144;
const MAX_CHUNK_SIZE: usize = 64 * 1024;
const MAX_MESSAGE_SIZE: usize = 256 * 1024;
const MAX_MESSAGES_PER_INPUT: usize = 8;
const MAX_FEED_CALLS: usize = 4_096;
const MAX_FRAGMENT_BYTES: usize = 64;

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
    let mut offset = 0;
    let mut messages = 0;
    let minimum_fragment = data.len().div_ceil(MAX_FEED_CALLS);
    let mut stopped_on_error = false;

    'feed: for _ in 0..MAX_FEED_CALLS {
        if offset == data.len() || messages == MAX_MESSAGES_PER_INPUT {
            break;
        }
        let fragment_size =
            (usize::from(data[offset] % MAX_FRAGMENT_BYTES as u8) + 1).max(minimum_fragment);
        let end = offset.saturating_add(fragment_size).min(data.len());
        let mut fragment = &data[offset..end];
        offset = end;

        loop {
            match deserializer.get_next_message(fragment) {
                Ok(Some(payload)) => {
                    messages += 1;
                    fragment = &[];
                    if let Ok(RtmpMessage::SetChunkSize { size }) = payload.to_rtmp_message()
                        && let Ok(size) = usize::try_from(size)
                    {
                        let _ = deserializer.set_max_chunk_size(size);
                    }
                    if messages == MAX_MESSAGES_PER_INPUT {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    stopped_on_error = true;
                    break 'feed;
                }
            }
        }
    }

    if stopped_on_error {
        return;
    }

    while messages < MAX_MESSAGES_PER_INPUT {
        match deserializer.get_next_message(&[]) {
            Ok(Some(payload)) => {
                messages += 1;
                if let Ok(RtmpMessage::SetChunkSize { size }) = payload.to_rtmp_message()
                    && let Ok(size) = usize::try_from(size)
                {
                    let _ = deserializer.set_max_chunk_size(size);
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
});
