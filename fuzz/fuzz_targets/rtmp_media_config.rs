#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };

    oxiroute_rtmp::fuzz_media_configuration(data.as_ref());
});
