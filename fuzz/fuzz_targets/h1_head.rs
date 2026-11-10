#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use oxiroute_h1_head::{
    Automatic, Class, Edits, HOP_BY_HOP_REQUEST, MAX_HEADERS, RequestHead, ResponseHead, Scan,
    ScanResponse, Value, names, scan_request, scan_response, write_request, write_response,
};

const MAX_INPUT_BYTES: usize = 131_072;
const ID: &[u8] = b"0123456789abcdef";

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let Some((selector, input)) = data.split_first() else {
        return;
    };
    if selector & 1 == 0 {
        request(input, usize::from(*selector >> 1));
    } else {
        response(input, usize::from(*selector >> 1));
    }
});

/// `Head` must agree with httparse, stay `Partial` on prefixes, ignore what follows the head, and write a head that
/// scans again.
fn request(input: &[u8], cut: usize) {
    let mut head = RequestHead::new();
    let mut probe = RequestHead::new();
    let scan = scan_request(input, &mut head);
    if scan != Scan::Head {
        return;
    }
    let len = head.head_len as usize;
    assert!(len <= input.len() && head.line_len as usize <= len);
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    assert_eq!(parsed.parse(input), Ok(httparse::Status::Complete(len)));
    assert_eq!(parsed.headers.len(), head.headers().len());
    for (theirs, ours) in parsed.headers.iter().zip(head.headers()) {
        assert_eq!(theirs.name.as_bytes(), ours.name(input));
        assert_eq!(theirs.value, ours.value(input));
    }
    assert_eq!(
        scan_request(&input[..len.saturating_sub(1)], &mut probe),
        Scan::Partial
    );
    assert_eq!(
        scan_request(&input[..cut.min(len - 1)], &mut probe),
        Scan::Partial
    );
    assert_eq!(scan_request(&input[..len], &mut probe), Scan::Head);
    assert_eq!(probe.facts, head.facts);

    let mut edits = Edits::new();
    for (bit, name) in [
        names::KEEP_ALIVE,
        names::PROXY_CONNECTION,
        names::PROXY_AUTHENTICATE,
        names::PROXY_AUTHORIZATION,
        names::TE,
        names::TRAILER,
    ]
    .into_iter()
    .enumerate()
    {
        if head.facts.hop_by_hop >> bit & 1 != 0 {
            edits.remove(name).unwrap();
        }
    }
    assert_eq!(HOP_BY_HOP_REQUEST, 0x3f);
    edits
        .set(names::X_REQUEST_ID, Value::new(ID).unwrap())
        .unwrap();
    let mut out = Vec::new();
    write_request(&mut out, input, &head, &edits);
    if head.headers().len() == MAX_HEADERS {
        return;
    }
    assert_eq!(scan_request(&out, &mut probe), Scan::Head);
    assert_eq!(probe.head_len as usize, out.len());
    assert_eq!(probe.method(&out), head.method(input));
    assert_eq!(probe.target(&out), head.target(input));
    assert_eq!(probe.host(&out), head.host(input));
    assert_eq!(probe.facts.request_id_count, 1);
    assert_eq!(probe.facts.hop_by_hop, 0);
}

fn response(input: &[u8], cut: usize) {
    let mut head = ResponseHead::new();
    let mut probe = ResponseHead::new();
    if scan_response(input, &mut head) != ScanResponse::Head {
        return;
    }
    let len = head.head_len as usize;
    assert!(len <= input.len() && head.line_len as usize <= len);
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Response::new(&mut headers);
    assert_eq!(parsed.parse(input), Ok(httparse::Status::Complete(len)));
    assert_eq!(parsed.code, Some(head.status));
    assert_eq!(parsed.headers.len(), head.headers().len());
    for (theirs, ours) in parsed.headers.iter().zip(head.headers()) {
        assert_eq!(theirs.name.as_bytes(), ours.name(input));
        assert_eq!(theirs.value, ours.value(input));
    }
    assert_eq!(
        scan_response(&input[..len.saturating_sub(1)], &mut probe),
        ScanResponse::Partial
    );
    assert_eq!(
        scan_response(&input[..cut.min(len - 1)], &mut probe),
        ScanResponse::Partial
    );

    let mut edits = Edits::new();
    edits.remove(names::CONNECTION).unwrap();
    edits
        .stamp_request_id(input, &head, Value::new(ID).unwrap())
        .unwrap();
    let automatic = Automatic {
        date: b"Mon, 09 Nov 2026 20:00:00 GMT",
        connection: b"keep-alive",
    };
    let mut out = Vec::new();
    write_response(&mut out, input, &head, &edits, 1, Some(automatic));
    if head.headers().len() > MAX_HEADERS - 4 {
        return;
    }
    assert_eq!(scan_response(&out, &mut probe), ScanResponse::Head);
    assert_eq!(probe.head_len as usize, out.len());
    assert_eq!(probe.status, head.status);
    assert_eq!(probe.reason(&out), head.reason(input));
    assert!(probe.facts.present & Class::XRequestId.bit() != 0);
}
