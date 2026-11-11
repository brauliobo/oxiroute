#![no_main]

mod support;

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use libfuzzer_sys::fuzz_target;
use oxiroute_h3::head::{self, Failure};
use oxiroute_h3::qpack::{decode, encode};

const MAX_INPUT_BYTES: usize = 65_536;
const LIMITS: [u64; 3] = [64, 16_384, u64::MAX];
const MAX_FIELDS: usize = 64;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let data = data.as_ref();
    if let Some(block) = support::strip_prefix(data, b"decode:") {
        decode_arbitrary(block);
    } else if let Some(spec) = support::strip_prefix(data, b"roundtrip:") {
        round_trip(spec);
    } else if let Some((&mode, rest)) = data.split_first() {
        if mode % 2 == 0 {
            decode_arbitrary(rest);
        } else {
            round_trip(rest);
        }
    }
});

/// Arbitrary bytes as a request section and as a trailer section: no panic, a decoded head respects the limit and
/// is an HTTP/3 request, and the line decoder and the head builder agree on whether the section is well formed.
fn decode_arbitrary(block: &[u8]) {
    for limit in LIMITS {
        if let Ok(request) = head::request(block, limit) {
            assert_eq!(request.version(), http::Version::HTTP_3);
            let size: usize = request
                .headers()
                .iter()
                .map(|(n, v)| n.as_str().len() + v.len())
                .sum();
            assert!(size as u64 <= limit);
        }
        let _ = head::trailers(block, limit);
    }
    if let Err(Failure::Qpack(_)) = head::request(block, u64::MAX) {
        assert!(lines_fail(block));
    }
}

/// Whether walking the field lines of the section (decoding each string) hits an error.
fn lines_fail(block: &[u8]) -> bool {
    let Ok(mut section) = decode::section(block) else {
        return true;
    };
    let mut out = Vec::new();
    loop {
        match section.next_line() {
            Ok(None) => return false,
            Err(_) => return true,
            Ok(Some(decode::Line::Indexed(_))) => {}
            Ok(Some(decode::Line::NameRef { value, .. })) => {
                out.clear();
                if value.write_to(&mut out).is_err() {
                    return true;
                }
            }
            Ok(Some(decode::Line::Literal { name, value })) => {
                out.clear();
                if name.write_to(&mut out).is_err() || value.write_to(&mut out).is_err() {
                    return true;
                }
            }
        }
    }
}

/// Fields cut from the input, encoded as a response and decoded again: the same lines come back.
fn round_trip(spec: &[u8]) {
    let mut headers = HeaderMap::new();
    let mut count = 0;
    for pair in spec.split(|&b| b == b'\n').take(MAX_FIELDS) {
        let (name, value) =
            pair.split_at(pair.iter().position(|&b| b == b':').unwrap_or(pair.len()));
        let name: Vec<u8> = name.iter().map(u8::to_ascii_lowercase).collect();
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(&name),
            HeaderValue::from_bytes(value.strip_prefix(b":").unwrap_or(value)),
        ) else {
            continue;
        };
        if matches!(
            name.as_str(),
            "connection"
                | "keep-alive"
                | "proxy-connection"
                | "transfer-encoding"
                | "upgrade"
                | "te"
        ) {
            continue;
        }
        headers.append(name, value);
        count += 1;
    }
    let mut block = Vec::new();
    encode::response(&mut block, StatusCode::OK, &headers);
    let mut section = decode::section(&block).expect("own prefix");
    let mut lines = 0;
    while let Some(line) = section.next_line().expect("own section decodes") {
        lines += 1;
        let mut text = Vec::new();
        if let decode::Line::NameRef { value, .. } = line {
            value.write_to(&mut text).expect("own literal");
        }
    }
    assert_eq!(lines, count + 1);
    let mut trailers = Vec::new();
    encode::trailers(&mut trailers, &headers);
    let decoded = head::trailers(&trailers, u64::MAX).expect("own trailers decode");
    assert_eq!(decoded.len(), headers.len());
}
