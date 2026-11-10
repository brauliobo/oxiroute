//! In-process cost of the head work of one proxied request: scan and write the request, scan and write the response,
//! with the edits a lane makes (hop-by-hop removal, request id, `Date` and `Connection`).
//!
//! `perf stat -e instructions:u hdrbench MODE ITERS`; the cost per iteration is the difference of two ITERS, as in
//! `benchmarks/r64-floor/src/bin/hdrbench.rs` (`run_hdrbench.sh` does that). MODE is `req`, `resp`, `scan` (both
//! scans only), `qscan` or `pscan` (one scan only) or `all`, followed by `3` (the 1 KiB lane heads) or `12` (browser-sized heads).

use oxiroute_h1_head::{
    Automatic, Class, Edits, RequestHead, ResponseHead, Scan, ScanResponse, Value, names,
    scan_request, scan_response, write_request, write_response,
};
use std::hint::black_box;

const SMALL_REQ: &[u8] =
    b"GET /b1k HTTP/1.1\r\nhost: bench.example.com\r\nuser-agent: r2tool\r\naccept: */*\r\n\r\n";
const BROWSER_REQ: &[u8] = b"GET /static/app.js?v=3 HTTP/1.1\r\nHost: www.example.com\r\nConnection: keep-alive\r\n\
User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36\r\n\
Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8\r\n\
Accept-Language: en-US,en;q=0.9\r\nAccept-Encoding: gzip, deflate, br\r\nCache-Control: max-age=0\r\n\
Cookie: session=0123456789abcdef0123456789abcdef; theme=dark; lang=en\r\nReferer: https://www.example.com/index.html\r\n\
Sec-Fetch-Dest: document\r\nSec-Fetch-Mode: navigate\r\nUpgrade-Insecure-Requests: 1\r\n\r\n";
const SMALL_RESP: &[u8] =
    b"HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: 1024\r\n\
date: Mon, 09 Nov 2026 20:00:00 GMT\r\n\r\n";
const BROWSER_RESP: &[u8] = b"HTTP/1.1 200 OK\r\nServer: origin\r\nDate: Mon, 09 Nov 2026 20:00:00 GMT\r\n\
Content-Type: application/javascript\r\nContent-Length: 1024\r\nConnection: keep-alive\r\nCache-Control: public, max-age=3600\r\n\
ETag: \"0123456789abcdef\"\r\nLast-Modified: Mon, 09 Nov 2026 10:00:00 GMT\r\nVary: Accept-Encoding\r\nX-Content-Type-Options: nosniff\r\n\r\n";
const DATE: &[u8] = b"Mon, 09 Nov 2026 20:00:00 GMT";

/// The fixed hop-by-hop names of a request, in the order of the bits of `RequestFacts::hop_by_hop`.
const HOP: [oxiroute_h1_head::Name<'static>; 6] = [
    names::KEEP_ALIVE,
    names::PROXY_CONNECTION,
    names::PROXY_AUTHENTICATE,
    names::PROXY_AUTHORIZATION,
    names::TE,
    names::TRAILER,
];

fn request(
    input: &'static [u8],
    head: &mut RequestHead,
    edits: &mut Edits<'static>,
    out: &mut Vec<u8>,
    id: &'static [u8],
    write: bool,
) {
    assert_eq!(scan_request(black_box(input), head), Scan::Head);
    if !write {
        return;
    }
    edits.clear();
    for (bit, name) in HOP.iter().enumerate() {
        if head.facts.hop_by_hop >> bit & 1 != 0 {
            edits.remove(*name).unwrap();
        }
    }
    if head.facts.connection != oxiroute_h1_head::Connection::Absent {
        edits.remove(names::CONNECTION).unwrap();
    }
    if head.facts.present & Class::XRequestId.bit() == 0 {
        edits.set(names::X_REQUEST_ID, Value::trusted(id)).unwrap();
    }
    write_request(out, input, head, edits);
}

fn response(
    input: &'static [u8],
    head: &mut ResponseHead,
    edits: &mut Edits<'static>,
    out: &mut Vec<u8>,
    id: &'static [u8],
    write: bool,
) {
    assert_eq!(scan_response(black_box(input), head), ScanResponse::Head);
    if !write {
        return;
    }
    edits.clear();
    for (bit, name) in HOP.iter().enumerate() {
        if head.facts.hop_by_hop >> bit & 1 != 0 {
            edits.remove(*name).unwrap();
        }
    }
    edits
        .stamp_request_id(input, head, Value::trusted(id))
        .unwrap();
    write_response(
        out,
        input,
        head,
        edits,
        1,
        Some(Automatic {
            date: DATE,
            connection: b"keep-alive",
        }),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mode, iters): (&str, u64) = (&args[1], args[2].parse().unwrap());
    let browser = mode.ends_with("12");
    let (req_in, resp_in) = if browser {
        (BROWSER_REQ, BROWSER_RESP)
    } else {
        (SMALL_REQ, SMALL_RESP)
    };
    let (mut req_head, mut resp_head) = (RequestHead::new(), ResponseHead::new());
    let mut edits = Edits::new();
    let mut out = Vec::with_capacity(4096);
    let id: &'static [u8] = b"0123456789abcdef";
    let mut total = 0usize;
    for i in 0..iters {
        out.clear();
        black_box(i);
        match mode.trim_end_matches(char::is_numeric) {
            "req" => request(req_in, &mut req_head, &mut edits, &mut out, id, true),
            "resp" => response(resp_in, &mut resp_head, &mut edits, &mut out, id, true),
            "qscan" => request(req_in, &mut req_head, &mut edits, &mut out, id, false),
            "pscan" => response(resp_in, &mut resp_head, &mut edits, &mut out, id, false),
            "scan" => {
                request(req_in, &mut req_head, &mut edits, &mut out, id, false);
                response(resp_in, &mut resp_head, &mut edits, &mut out, id, false);
            }
            _ => {
                request(req_in, &mut req_head, &mut edits, &mut out, id, true);
                response(resp_in, &mut resp_head, &mut edits, &mut out, id, true);
            }
        }
        total += out.len() + usize::from(req_head.count);
    }
    black_box(total);
}
