#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use oxiroute_forward_proxy::{parse_absolute_form, parse_connect_authority};

const MAX_INPUT_BYTES: usize = 16 * 1024;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let data = data.as_ref();
    let (parser, data) = if let Some(data) = support::strip_prefix(data, b"absolute:") {
        (0, data)
    } else if let Some(data) = support::strip_prefix(data, b"connect:") {
        (1, data)
    } else {
        (
            usize::from(data.first().copied().unwrap_or_default() & 1),
            data.get(1..).unwrap_or_default(),
        )
    };
    let target = String::from_utf8_lossy(data);

    if parser == 0 {
        let _ = parse_absolute_form(target.as_ref());
    } else {
        let _ = parse_connect_authority(target.as_ref());
    }
});
