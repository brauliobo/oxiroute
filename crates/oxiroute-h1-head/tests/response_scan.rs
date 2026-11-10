//! Differential tests of `scan_response` against the classic client parse: `Head` must imply the classic path
//! accepts the same bytes and reads the same status, version, reason, header list and facts; `Slow` is never wrong.

mod common;

use common::classic_request::buf_to_content_length;
use common::classic_response::{ClassicResponse, classic_response};
use common::request_corpus::crlf;
use common::response_corpus::{Verdict, corpus};
use common::{MUTATION_ALPHABET, Rng, mutate};
use oxiroute_h1_head::{Class, Connection, ResponseHead, ScanResponse, scan_response};

fn assert_same_as_classic(buf: &[u8], head: &ResponseHead, classic: &ClassicResponse) {
    let shown = String::from_utf8_lossy(buf);
    assert_eq!(
        head.head_len as usize, classic.head_len,
        "head length of {shown:?}"
    );
    assert_eq!(
        head.status,
        classic.header.status.as_u16(),
        "status of {shown:?}"
    );
    assert_eq!(head.minor, classic.minor);
    assert_eq!(
        head.reason(buf),
        classic.reason.as_bytes(),
        "reason of {shown:?}"
    );
    assert_eq!(
        head.reason(buf),
        classic.header.get_reason_phrase().unwrap_or("").as_bytes(),
        "written reason of {shown:?}"
    );
    let list: Vec<(String, Vec<u8>)> = head
        .headers()
        .iter()
        .map(|h| {
            (
                String::from_utf8(h.name(buf).to_vec()).unwrap(),
                h.value(buf).to_vec(),
            )
        })
        .collect();
    assert_eq!(list, classic.arrival, "header list of {shown:?}");
    let named = |n: &str| {
        classic
            .arrival
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(n))
            .count()
    };
    let facts = &head.facts;
    assert_eq!(
        facts.transfer_encoding, classic.facts.transfer_encoding,
        "te of {shown:?}"
    );
    if facts.transfer_encoding {
        let chunked =
            common::classic_request::is_chunked_encoding_from_headers(&classic.header.headers);
        assert_eq!(facts.chunked_final, chunked, "chunked of {shown:?}");
    } else {
        assert!(!facts.chunked_final);
    }
    let classic_cl = classic
        .header
        .headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| buf_to_content_length(v.as_bytes()));
    assert_eq!(
        facts.content_length, classic_cl,
        "content-length of {shown:?}"
    );
    match facts.connection {
        Connection::Absent => assert_eq!(named("connection"), 0),
        Connection::KeepAlive | Connection::Close => {
            let wanted: &[u8] = if facts.connection == Connection::Close {
                b"close"
            } else {
                b"keep-alive"
            };
            assert_eq!(named("connection"), 1);
            assert!(classic.arrival.iter().any(
                |(n, v)| n.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case(wanted)
            ));
        }
        Connection::Other => panic!("an unusual Connection must be Slow: {shown:?}"),
    }
    assert_eq!(facts.upgrade, named("upgrade") > 0);
    let hop = [
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "connection",
        "transfer-encoding",
        "upgrade",
    ]
    .iter()
    .enumerate()
    .fold(0u16, |mask, (bit, n)| {
        mask | (u16::from(named(n) > 0) << bit)
    });
    assert_eq!(facts.hop_by_hop, hop, "hop-by-hop mask of {shown:?}");
    assert_eq!(facts.request_id, named("x-request-id") > 0);
    assert_eq!(
        facts.upstream_request_id,
        named("x-upstream-request-id") > 0
    );
    assert_eq!(
        facts.informational,
        classic.header.status.is_informational()
    );
    assert_eq!(
        facts.has_underscore,
        classic.arrival.iter().any(|(n, _)| n.contains('_'))
    );
    assert_eq!(
        facts.has_uppercase,
        classic
            .arrival
            .iter()
            .any(|(n, _)| n.bytes().any(|b| b.is_ascii_uppercase()))
    );
    let mut names: Vec<String> = classic
        .arrival
        .iter()
        .map(|(n, _)| n.to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(
        facts.duplicate_names,
        names.len() != classic.arrival.len(),
        "duplicates of {shown:?}"
    );
    for (h, (name, _)) in head.headers().iter().zip(&classic.arrival) {
        let lower = http::HeaderName::from_bytes(name.as_bytes()).unwrap();
        let known = [
            Class::KeepAlive,
            Class::Connection,
            Class::TransferEncoding,
            Class::Upgrade,
            Class::ContentLength,
            Class::Date,
        ];
        let expect = known
            .into_iter()
            .find(|c| c.lower() == lower.as_str().as_bytes())
            .unwrap_or(h.class);
        assert_eq!(h.class, expect);
        assert_eq!(h.class.lower().is_empty(), h.class == Class::Other);
        if h.class != Class::Other {
            assert_eq!(h.class.lower(), lower.as_str().as_bytes());
        }
    }
}

/// Scans `buf`; on `Head` checks it against the classic parse, the prefixes and a trailing payload.
fn check(buf: &[u8], head: &mut ResponseHead) -> ScanResponse {
    let scan = scan_response(buf, head);
    if scan == ScanResponse::Head {
        let classic = classic_response(buf).unwrap_or_else(|e| {
            panic!(
                "head accepted but classic says {e:?}: {:?}",
                String::from_utf8_lossy(buf)
            )
        });
        assert_same_as_classic(buf, head, &classic);
        let len = head.head_len as usize;
        let mut probe = ResponseHead::new();
        for cut in 0..len {
            assert_eq!(
                scan_response(&buf[..cut], &mut probe),
                ScanResponse::Partial,
                "prefix {cut} of {:?}",
                String::from_utf8_lossy(&buf[..len])
            );
        }
        let mut extended = buf[..len].to_vec();
        extended.extend_from_slice(b"HTTP/1.1 200 OK\r\n\x00\r\n\r\n\xff");
        assert_eq!(scan_response(&extended, &mut probe), ScanResponse::Head);
        assert_eq!(probe.head_len, head.head_len);
        assert_eq!(probe.headers(), head.headers());
        assert_eq!(probe.facts, head.facts);
    }
    scan
}

#[test]
fn corpus_matches_expectations_and_classic() {
    let mut head = ResponseHead::new();
    for (name, bytes, verdict) in corpus() {
        let scan = check(&bytes, &mut head);
        match verdict {
            Verdict::Head => assert_eq!(scan, ScanResponse::Head, "{name}"),
            Verdict::Slow => assert_eq!(scan, ScanResponse::Slow, "{name}"),
        }
    }
}

#[test]
fn facts_of_a_typical_head() {
    let mut head = ResponseHead::new();
    let buf = crlf(
        "HTTP/1.1 301 Moved\nContent-Length: 9\nConnection: close\nX-Request-Id: u\nTE: x\nUpgrade: h2c\n\n",
    );
    assert_eq!(scan_response(&buf, &mut head), ScanResponse::Head);
    assert_eq!((head.status, head.minor), (301, 1));
    assert_eq!(head.reason(&buf), b"Moved");
    assert_eq!(head.facts.content_length, Some(9));
    assert_eq!(head.facts.connection, Connection::Close);
    assert!(head.facts.request_id && !head.facts.upstream_request_id && !head.facts.informational);
    assert_eq!(
        head.facts.hop_by_hop,
        oxiroute_h1_head::HOP_TE | oxiroute_h1_head::HOP_CONNECTION | oxiroute_h1_head::HOP_UPGRADE
    );
    assert_eq!(head.find(Class::XRequestId).unwrap().value(&buf), b"u");
    assert_eq!(head.head_len as usize, buf.len());
}

#[test]
fn short_buffers_are_partial() {
    let mut head = ResponseHead::new();
    for buf in [
        &b""[..],
        b"H",
        b"HTTP/1.",
        b"HTTP/1.1",
        b"HTTP/1.1 ",
        b"HTTP/1.1 2",
        b"HTTP/1.1 200",
        b"HTTP/1.1 200 ",
        b"HTTP/1.1 200 OK\r",
    ] {
        assert_eq!(
            scan_response(buf, &mut head),
            ScanResponse::Partial,
            "{buf:?}"
        );
    }
}

const SEEDS: [&str; 7] = [
    "HTTP/1.1 200 OK\ncontent-type: application/octet-stream\ncontent-length: 1024\ndate: Mon, 09 Nov 2026 20:00:00 GMT\n\n",
    "HTTP/1.1 200 OK\nServer: origin\nContent-Length: 5\nConnection: keep-alive\nKeep-Alive: timeout=5\nX-A:  padded \t\n\nhello",
    "HTTP/1.1 200 OK\nTransfer-Encoding: gzip, chunked\nContent-Length: 4\nSet-Cookie: a=1\nSet-Cookie: b=2\nX-Request-Id: 1\n\n",
    "HTTP/1.0 404 Not Found\nVary: a\nX-B: 1\nVary: b\nUpgrade: h2c\nTE: x\nTrailer: y\n\n",
    "HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\nConnection: Upgrade\n\n",
    "HTTP/1.1 204 \nX-Upstream-Request-Id: u\nx-request-id: v\nProxy-Connection: x\nX_A: 1\n\n",
    "HTTP/1.1 200 OK\nA:\nB: \nC:\t\nContent-Length: 0\n\n",
];

#[test]
fn mutated_heads_never_head_unless_classic_agrees() {
    let seeds: Vec<Vec<u8>> = SEEDS.iter().map(|s| crlf(s)).collect();
    let mut rng = Rng(0xA5A5_5A5A_1234_5678);
    let mut head = ResponseHead::new();
    let (mut heads, mut slow) = (0, 0);
    for round in 0..400_000usize {
        let mut buf = seeds[round % seeds.len()].clone();
        mutate(&mut rng, &mut buf, MUTATION_ALPHABET);
        match check(&buf, &mut head) {
            ScanResponse::Head => heads += 1,
            ScanResponse::Slow => slow += 1,
            ScanResponse::Partial => {}
        }
    }
    eprintln!("mutations: {heads} heads, {slow} slow");
    assert!(heads > 20_000 && slow > 20_000);
}

#[test]
fn every_single_byte_substitution_of_the_seeds_is_judged_like_classic() {
    let mut head = ResponseHead::new();
    for seed in SEEDS {
        let seed = crlf(seed);
        for at in 0..seed.len() {
            for byte in 0..=255u8 {
                let mut buf = seed.clone();
                buf[at] = byte;
                check(&buf, &mut head);
            }
        }
    }
}
