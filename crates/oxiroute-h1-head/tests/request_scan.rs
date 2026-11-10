//! Differential tests of `scan_request` against the classic parse: `Head` must imply the classic path accepts the
//! same bytes and reads the same method, target, version, header list and facts.

mod common;

use common::classic_request::{ClassicRequest, classic_request};
use common::request_corpus::{Expect, corpus, crlf};
use common::{MUTATION_ALPHABET, Rng, mutate};
use oxiroute_h1_head::{Class, Connection, RequestHead, Scan, scan_request};

fn assert_same_as_classic(buf: &[u8], head: &RequestHead, classic: &ClassicRequest) {
    let shown = String::from_utf8_lossy(buf);
    assert_eq!(
        head.head_len as usize, classic.head_len,
        "head length of {shown:?}"
    );
    assert_eq!(
        head.method(buf),
        classic.header.method.as_str().as_bytes(),
        "method of {shown:?}"
    );
    assert_eq!(
        head.target(buf),
        classic.header.raw_path(),
        "target of {shown:?}"
    );
    assert_eq!(
        (head.minor, classic.minor, classic.header.version),
        (1, 1, http::Version::HTTP_11)
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
    for (h, (name, _)) in head.headers().iter().zip(&classic.arrival) {
        let lower = http::HeaderName::from_bytes(name.as_bytes()).unwrap();
        assert_eq!(h.class, class_of(&lower), "class of {name:?}");
    }
    let named = |n: &str| {
        classic
            .arrival
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(n))
            .count()
    };
    let facts = &head.facts;
    assert_eq!(named("host"), 1);
    assert!(
        head.headers()[usize::from(facts.host)]
            .name(buf)
            .eq_ignore_ascii_case(b"host")
    );
    assert_eq!(
        facts.transfer_encoding, classic.facts.transfer_encoding,
        "te of {shown:?}"
    );
    assert_eq!(facts.upgrade, classic.facts.upgrade);
    assert_eq!(facts.upgrade, named("upgrade") > 0);
    assert_eq!(facts.expect, named("expect") > 0);
    let classic_cl = classic
        .header
        .headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| common::classic_request::buf_to_content_length(v.as_bytes()));
    assert_eq!(
        facts.content_length, classic_cl,
        "content-length of {shown:?}"
    );
    match facts.connection {
        Connection::Absent => assert_eq!(named("connection"), 0),
        Connection::KeepAlive => {
            assert!(named("connection") == 1 && has_token(&classic.arrival, "keep-alive"));
        }
        Connection::Close => {
            assert!(named("connection") == 1 && has_token(&classic.arrival, "close"));
        }
        Connection::Other => {}
    }
    let hop = [
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
    ]
    .iter()
    .enumerate()
    .fold(0u8, |mask, (bit, n)| mask | (u8::from(named(n) > 0) << bit));
    assert_eq!(facts.hop_by_hop, hop, "hop-by-hop mask of {shown:?}");
    assert_eq!(classic.facts.hop_by_hop, hop != 0);
    assert_eq!(usize::from(facts.request_id_count), named("x-request-id"));
    if named("x-request-id") > 0 {
        let first = classic
            .arrival
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case("x-request-id"))
            .unwrap();
        assert_eq!(usize::from(facts.request_id), first);
    }
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
}

fn class_of(name: &http::HeaderName) -> Class {
    [
        Class::KeepAlive,
        Class::ProxyConnection,
        Class::ProxyAuthenticate,
        Class::ProxyAuthorization,
        Class::Te,
        Class::Trailer,
        Class::Connection,
        Class::TransferEncoding,
        Class::Upgrade,
        Class::Host,
        Class::ContentLength,
        Class::Expect,
        Class::XRequestId,
        Class::XUpstreamRequestId,
        Class::Date,
        Class::Server,
        Class::SetCookie,
        Class::CacheControl,
        Class::ContentType,
        Class::ContentEncoding,
        Class::AcceptRanges,
        Class::Age,
    ]
    .into_iter()
    .find(|c| c.lower() == name.as_str().as_bytes())
    .unwrap_or(Class::Other)
}

fn has_token(arrival: &[(String, Vec<u8>)], token: &str) -> bool {
    arrival.iter().any(|(n, v)| {
        n.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case(token.as_bytes())
    })
}

/// Scans `buf`; on `Head` checks it against the classic parse, the prefixes and a trailing payload.
fn check(buf: &[u8], head: &mut RequestHead) -> Scan {
    let scan = scan_request(buf, head);
    if scan == Scan::Head {
        let classic = classic_request(buf).unwrap_or_else(|e| {
            panic!(
                "head accepted but classic says {e:?}: {:?}",
                String::from_utf8_lossy(buf)
            )
        });
        assert_same_as_classic(buf, head, &classic);
        let len = head.head_len as usize;
        let mut probe = RequestHead::new();
        for cut in 0..len {
            assert_eq!(
                scan_request(&buf[..cut], &mut probe),
                Scan::Partial,
                "prefix {cut} of {:?}",
                String::from_utf8_lossy(&buf[..len])
            );
        }
        let mut extended = buf[..len].to_vec();
        extended.extend_from_slice(b"GET /next HTTP/1.1\r\n\x00\r\n\r\n\xff");
        assert_eq!(scan_request(&extended, &mut probe), Scan::Head);
        assert_eq!(probe.head_len, head.head_len);
        assert_eq!(probe.headers(), head.headers());
        assert_eq!(probe.facts, head.facts);
    }
    scan
}

#[test]
fn corpus_matches_expectations_and_classic() {
    let mut head = RequestHead::new();
    for (name, bytes, expect) in corpus() {
        let scan = check(&bytes, &mut head);
        match expect {
            Expect::Head => assert_eq!(scan, Scan::Head, "{name}"),
            Expect::Miss(reason) => assert_eq!(scan, Scan::Miss(reason), "{name}"),
        }
    }
}

#[test]
fn facts_of_a_browser_head() {
    let mut head = RequestHead::new();
    let buf = crlf(
        "GET /a?b HTTP/1.1\nHost: h\nConnection: close\nX-Request-Id: 7\nTE: trailers\nContent-Length: 12\nX_A: 1\n\n",
    );
    assert_eq!(scan_request(&buf, &mut head), Scan::Head);
    assert_eq!(head.method(&buf), b"GET");
    assert_eq!(head.target(&buf), b"/a?b");
    assert_eq!(head.host(&buf), b"h");
    assert_eq!(head.facts.connection, Connection::Close);
    assert_eq!(head.facts.content_length, Some(12));
    assert_eq!(head.facts.hop_by_hop, oxiroute_h1_head::HOP_TE);
    assert_eq!((head.facts.request_id_count, head.facts.request_id), (1, 2));
    assert!(head.facts.has_underscore && head.facts.has_uppercase && !head.facts.duplicate_names);
    assert_eq!(head.head_len as usize, buf.len());
    assert_eq!(head.facts.present & Class::Host.bit(), Class::Host.bit());
}

#[test]
fn empty_and_short_buffers_are_partial() {
    let mut head = RequestHead::new();
    for buf in [
        &b""[..],
        b"G",
        b"GET",
        b"GET ",
        b"GET /",
        b"GET / HTTP/1.",
        b"GET / HTTP/1.1",
        b"GET / HTTP/1.1\r",
    ] {
        assert_eq!(scan_request(buf, &mut head), Scan::Partial, "{buf:?}");
    }
}

const SEEDS: [&str; 8] = [
    "GET /b1k HTTP/1.1\nhost: bench.example.com\nuser-agent: r2tool\naccept: */*\n\n",
    "POST /a?b=c HTTP/1.1\nHost: x\nContent-Length: 5\nX-Pad:  padded value \t\n\nhello",
    "GET /p HTTP/1.1\nHost: a\nCookie: a=b; c=d\tz\nConnection: keep-alive\n\n",
    "POST /u HTTP/1.1\nHost: a\nTransfer-Encoding: gzip, chunked\nContent-Length: 4\n\n",
    "GET / HTTP/1.1\nHost: a\nX-A: 1\nx-a: 2\nAccept: y\nX-A: 3\nUpgrade: websocket\nConnection: Upgrade\n\n",
    "PUT /u HTTP/1.1\nHost: a\nExpect: 100-continue\nContent-Length: 3\nTE: trailers\nKeep-Alive: 5\n\nabc",
    "GET /a/b/c?d=e&f=g HTTP/1.1\nHost: a:8080\nX-Request-Id: abc\nX_Under: 1\nProxy-Authorization: Basic Zm9v\n\n",
    "OPTIONS / HTTP/1.1\nHost:\nA:\nB: \nC:\t\n\n",
];

#[test]
fn mutated_heads_never_head_unless_classic_agrees() {
    let seeds: Vec<Vec<u8>> = SEEDS.iter().map(|s| crlf(s)).collect();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut head = RequestHead::new();
    let (mut heads, mut misses, mut partials) = (0, 0, 0);
    for round in 0..400_000usize {
        let mut buf = seeds[round % seeds.len()].clone();
        mutate(&mut rng, &mut buf, MUTATION_ALPHABET);
        match check(&buf, &mut head) {
            Scan::Head => heads += 1,
            Scan::Miss(_) => misses += 1,
            Scan::Partial => partials += 1,
        }
    }
    eprintln!("mutations: {heads} heads, {misses} misses, {partials} partials");
    assert!(
        heads > 20_000 && misses > 20_000,
        "the corpus must exercise both outcomes"
    );
}

#[test]
fn every_single_byte_substitution_of_the_seeds_is_judged_like_classic() {
    let mut head = RequestHead::new();
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

#[test]
fn scan_is_independent_of_stale_state_in_the_head() {
    let mut reused = RequestHead::new();
    let mut rng = Rng(7);
    for round in 0..20_000usize {
        let mut buf = crlf(SEEDS[round % SEEDS.len()]);
        mutate(&mut rng, &mut buf, MUTATION_ALPHABET);
        let mut fresh = RequestHead::new();
        let (a, b) = (
            scan_request(&buf, &mut reused),
            scan_request(&buf, &mut fresh),
        );
        assert_eq!(a, b);
        if a == Scan::Head {
            assert_eq!(reused.headers(), fresh.headers());
            assert_eq!(reused.facts, fresh.facts);
            assert_eq!(
                (reused.method, reused.target, reused.head_len),
                (fresh.method, fresh.target, fresh.head_len)
            );
        }
    }
}

#[test]
fn every_byte_in_every_position_of_the_request_line_is_judged_like_classic() {
    let mut head = RequestHead::new();
    let mut accepted_in_target = Vec::new();
    for byte in 0..=255u8 {
        for template in [
            "GET /a{}b HTTP/1.1\r\nHost: a\r\n\r\n",
            "GET /a?b{}c HTTP/1.1\r\nHost: a\r\n\r\n",
            "GET /{} HTTP/1.1\r\nHost: a\r\n\r\n",
        ] {
            let mut buf = template.as_bytes().to_vec();
            let at = buf.iter().position(|&b| b == b'{').unwrap();
            buf.splice(at..at + 2, [byte]);
            if check(&buf, &mut head) == Scan::Head && template.contains("/a{") {
                accepted_in_target.push(byte);
            }
        }
        let mut method = b"GET / HTTP/1.1\r\nHost: a\r\n\r\n".to_vec();
        method[1] = byte;
        check(&method, &mut head);
        let mut name = b"GET / HTTP/1.1\r\nHost: a\r\nXzY: v\r\n\r\n".to_vec();
        let at = name.iter().position(|&b| b == b'z').unwrap();
        name[at] = byte;
        check(&name, &mut head);
        let mut value = b"GET / HTTP/1.1\r\nHost: a\r\nX: vzv\r\n\r\n".to_vec();
        let at = value.iter().position(|&b| b == b'z').unwrap();
        value[at] = byte;
        check(&value, &mut head);
    }
    let expected: Vec<u8> = (0..=255u8)
        .filter(|b| matches!(b, 0x21 | 0x24..=0x3b | 0x3d | 0x3f..=0x5f | 0x61..=0x7e))
        .collect();
    assert_eq!(accepted_in_target, expected);
}
