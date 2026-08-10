#![no_main]

mod support;

use std::io::Cursor;

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use rml_rtmp::{messages::MessagePayload, rml_amf0, time::RtmpTimestamp};

const MAX_INPUT_BYTES: usize = 32_768;

fuzz_target!(|data: &[u8]| {
    let Some(mut data) =
        support::bounded_input(data, MAX_INPUT_BYTES).map(|data| data.into_owned())
    else {
        return;
    };
    if data == b"seed:amf" {
        data = vec![0x02, 0x00, 0x04, b't', b'e', b's', b't'];
    }

    let mut cursor = Cursor::new(data.as_slice());
    let _ = rml_amf0::deserialize(&mut cursor);
    for type_id in [15, 17, 18, 20] {
        let payload = MessagePayload {
            timestamp: RtmpTimestamp::new(0),
            type_id,
            message_stream_id: 1,
            data: Bytes::copy_from_slice(&data),
        };
        let _ = payload.to_rtmp_message();
    }
});
