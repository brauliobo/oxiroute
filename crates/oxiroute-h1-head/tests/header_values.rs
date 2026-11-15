//! The shared tab-aware scanner preserves HTTP/1 value bytes, error reasons and partial-head behavior.

use oxiroute_h1_head::{
    MissReason, RequestHead, ResponseHead, Scan, ScanResponse, is_header_value, scan_request,
    scan_response,
};

fn heads(value: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut request = b"GET / HTTP/1.1\r\nHost: example.com\r\nX-Value: ".to_vec();
    request.extend_from_slice(value);
    request.extend_from_slice(b"\r\n\r\n");
    let mut response = b"HTTP/1.1 200 OK\r\nX-Value: ".to_vec();
    response.extend_from_slice(value);
    response.extend_from_slice(b"\r\n\r\n");
    (request, response)
}

#[test]
fn every_value_byte_at_vector_boundaries_keeps_the_same_grammar() {
    for at in [1, 15, 16, 31, 32, 63, 64, 127, 128] {
        let mut value = vec![b'a'; at + 2];
        value[at + 1] = b'z';
        for b in 0..=255u8 {
            value[at] = b;
            let valid = http::HeaderValue::from_bytes(&value).is_ok();
            assert_eq!(is_header_value(&value), valid);
            let (request, response) = heads(&value);
            let mut req = RequestHead::new();
            let mut resp = ResponseHead::new();
            let reason = match b {
                b'\r' => MissReason::BareCr,
                b'\n' => MissReason::BareLf,
                _ => MissReason::Control,
            };
            assert_eq!(
                scan_request(&request, &mut req),
                if valid {
                    Scan::Head
                } else {
                    Scan::Miss(reason)
                }
            );
            assert_eq!(
                scan_response(&response, &mut resp),
                if valid {
                    ScanResponse::Head
                } else {
                    ScanResponse::Slow
                }
            );
            if valid {
                assert_eq!(req.headers()[1].value(&request), value);
                assert_eq!(resp.headers()[0].value(&response), value);
            }
        }
    }
}

#[test]
fn tab_runs_and_high_bytes_remain_partial_at_every_split_and_ignore_body_bytes() {
    let mut value = vec![b'\t'; 192];
    value[0] = b'a';
    value[64] = 0x80;
    value[128] = 0xff;
    value[191] = b'z';
    let (mut request, mut response) = heads(&value);
    let mut req = RequestHead::new();
    let mut resp = ResponseHead::new();
    for end in 0..request.len() {
        assert_eq!(
            scan_request(&request[..end], &mut req),
            Scan::Partial,
            "request split {end}"
        );
    }
    for end in 0..response.len() {
        assert_eq!(
            scan_response(&response[..end], &mut resp),
            ScanResponse::Partial,
            "response split {end}"
        );
    }
    let request_len = request.len();
    let response_len = response.len();
    request.extend_from_slice(b"\0\x7f\r\n");
    response.extend_from_slice(b"\0\x7f\r\n");
    assert_eq!(scan_request(&request, &mut req), Scan::Head);
    assert_eq!(scan_response(&response, &mut resp), ScanResponse::Head);
    assert_eq!(req.head_len as usize, request_len);
    assert_eq!(resp.head_len as usize, response_len);
    assert_eq!(req.headers()[1].value(&request), value);
    assert_eq!(resp.headers()[0].value(&response), value);
}
