#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use rml_rtmp::handshake::{Handshake, PeerType};

const HANDSHAKE_BLOCK_SIZE: usize = 1536;
const RTMP_VERSION: u8 = 3;
const MAX_INPUT_BYTES: usize = 128 * 1024;

fuzz_target!(|data: &[u8]| {
    let Some(mut data) =
        support::bounded_input(data, MAX_INPUT_BYTES).map(|data| data.into_owned())
    else {
        return;
    };
    if data == b"seed:simple" {
        data = vec![RTMP_VERSION; HANDSHAKE_BLOCK_SIZE + 1];
    }

    let mut handshake = Handshake::new(PeerType::Server);
    let split = data.len() / 2;
    if handshake.process_bytes(&data[..split]).is_ok() {
        let _ = handshake.process_bytes(&data[split..]);
    }
});
