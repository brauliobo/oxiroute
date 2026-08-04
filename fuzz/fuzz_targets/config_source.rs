#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use oxiroute_config::load_lua;
use oxiroute_config_source::{ConfigFormat, decode_value, expand_templates, render_value};

const MAX_INPUT_BYTES: usize = 128 * 1024;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let data = data.as_ref();
    let (format, source) = select_format(data);

    match format {
        ConfigFormat::Lua => {
            if let Ok(source) = std::str::from_utf8(source) {
                let _ = load_lua(source);
            }
        }
        format => {
            if let Ok(value) = decode_value(format, source) {
                let _ = expand_templates(&value);
                let _ = render_value(format, &value);
            }
        }
    }
});

fn select_format(data: &[u8]) -> (ConfigFormat, &[u8]) {
    for (prefix, format) in [
        (b"kdl:".as_slice(), ConfigFormat::Kdl),
        (b"lua:".as_slice(), ConfigFormat::Lua),
        (b"uci:".as_slice(), ConfigFormat::Uci),
        (b"hocon:".as_slice(), ConfigFormat::Hocon),
    ] {
        if let Some(source) = support::strip_prefix(data, prefix) {
            return (format, source);
        }
    }

    let selector = data.first().copied().unwrap_or_default() % 4;
    let source = data.get(1..).unwrap_or_default();
    let format = match selector {
        0 => ConfigFormat::Kdl,
        1 => ConfigFormat::Lua,
        2 => ConfigFormat::Uci,
        _ => ConfigFormat::Hocon,
    };
    (format, source)
}
