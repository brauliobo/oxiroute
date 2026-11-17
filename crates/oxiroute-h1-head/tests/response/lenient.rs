//! `read_response_head` against the classic client parse of the vendored sources (`httparse` with spaces before the
//! colon and obsolete folding accepted, `header_value_from_raw`, the `Content-Length` and `Connection` rules): what the
//! classic path accepts is read the same, what it refuses is refused, and every proper prefix of an accepted head is
//! partial.

use crate::common::classic::response::{ClassicResponse, classic_response, framing_length};
use crate::common::request_corpus::crlf;
use crate::common::{MUTATION_ALPHABET, Rng, mutate};
use oxiroute_h1_head::{
    Canonical, Class, Connection, HeadRead, MAX_NAME_LEN, MAX_NOMINATED, MAX_RESPONSE_HEADERS,
    ResponseHead, ScanResponse, canonicalize_response, read_response_head, scan_response,
    scan_response_lenient,
};

#[derive(Debug, PartialEq, Eq)]
enum Oracle {
    Partial,
    Refused,
    Head(usize),
}

fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn connection_values(classic: &ClassicResponse) -> Vec<Vec<u8>> {
    classic
        .header
        .headers
        .get_all(http::header::CONNECTION)
        .iter()
        .map(|value| value.as_bytes().to_vec())
        .collect()
}

fn is_reserved(option: &str) -> bool {
    ["close", "keep-alive", "upgrade"]
        .iter()
        .any(|reserved| option.eq_ignore_ascii_case(reserved))
}

/// The options of every `Connection` line, or `None` where the classic hop-by-hop removal refuses the response.
fn options_of(classic: &ClassicResponse) -> Option<Vec<String>> {
    let mut options = Vec::new();
    for value in connection_values(classic) {
        let line = std::str::from_utf8(&value).ok()?;
        if !value
            .iter()
            .all(|&b| b == b'\t' || (b' '..=b'~').contains(&b))
        {
            return None;
        }
        for option in line.split(',').map(str::trim).filter(|o| !o.is_empty()) {
            options.push(option.to_owned());
        }
    }
    for option in options.iter().filter(|o| !is_reserved(o)) {
        let name = http::HeaderName::from_bytes(option.as_bytes()).ok()?;
        if matches!(
            name,
            http::header::CONTENT_LENGTH | http::header::TRANSFER_ENCODING | http::header::HOST
        ) {
            return None;
        }
    }
    Some(options)
}

fn nominated(options: &[String]) -> Vec<String> {
    options
        .iter()
        .filter(|o| !is_reserved(o))
        .cloned()
        .collect()
}

fn keep_alive(classic: &ClassicResponse) -> Connection {
    let Some(first) = connection_values(classic).into_iter().next() else {
        return Connection::Absent;
    };
    if first.eq_ignore_ascii_case(b"keep-alive") {
        return Connection::KeepAlive;
    }
    let text = String::from_utf8_lossy(&first).into_owned();
    if text
        .split(',')
        .map(str::trim)
        .any(|option| option.eq_ignore_ascii_case("close"))
    {
        Connection::Close
    } else {
        Connection::Absent
    }
}

struct Verdict {
    oracle: Oracle,
    classic: Option<ClassicResponse>,
}

fn classify_with_classic(raw: &[u8]) -> Verdict {
    use crate::common::classic::request::Classic;
    let classic = match classic_response(raw) {
        Ok(classic) => classic,
        Err(Classic::Partial) => {
            return Verdict {
                oracle: Oracle::Partial,
                classic: None,
            };
        }
        Err(_) => {
            return Verdict {
                oracle: Oracle::Refused,
                classic: None,
            };
        }
    };
    let carried = classic.arrival.len() <= MAX_RESPONSE_HEADERS
        && classic
            .arrival
            .iter()
            .all(|(name, _)| name.len() <= MAX_NAME_LEN && name.bytes().all(is_token))
        && options_of(&classic).is_some_and(|options| nominated(&options).len() <= MAX_NOMINATED);
    let oracle = if carried {
        Oracle::Head(classic.head_len)
    } else {
        Oracle::Refused
    };
    Verdict {
        oracle,
        classic: Some(classic),
    }
}

fn read(raw: &[u8]) -> (HeadRead, ResponseHead, Vec<u8>) {
    let mut head = ResponseHead::new();
    let mut canonical = Vec::new();
    let read = read_response_head(raw, &mut head, &mut canonical);
    (read, head, canonical)
}

fn assert_reads_as_classic(raw: &[u8], classic: &ClassicResponse) {
    let shown = String::from_utf8_lossy(raw).into_owned();
    let (outcome, head, canon) = read(raw);
    let HeadRead::Head { canonical, raw_len } = outcome else {
        panic!("{outcome:?} for {shown:?}");
    };
    assert_eq!(raw_len, classic.head_len, "{shown:?}");
    let bytes: &[u8] = if canonical { &canon } else { raw };
    assert_eq!(
        head.head_len as usize,
        if canonical { canon.len() } else { raw_len }
    );
    assert_eq!(head.status, classic.header.status.as_u16(), "{shown:?}");
    assert_eq!(head.minor, classic.minor, "{shown:?}");
    assert_eq!(head.reason(bytes), classic.reason.as_bytes(), "{shown:?}");
    assert_eq!(head.headers().len(), classic.arrival.len(), "{shown:?}");
    let classic_values: Vec<_> = classic
        .arrival
        .iter()
        .map(|(name, _)| name.as_bytes())
        .collect();
    let ours: Vec<_> = head.headers().iter().map(|h| h.name(bytes)).collect();
    assert_eq!(ours, classic_values, "names of {shown:?}");
    for (h, (name, _)) in head.headers().iter().zip(&classic.arrival) {
        let stored: Vec<Vec<u8>> = classic
            .header
            .headers
            .get_all(http::HeaderName::from_bytes(name.as_bytes()).unwrap())
            .iter()
            .map(|v| v.as_bytes().to_vec())
            .collect();
        let dropped = h.class == Class::ContentLength && classic.facts.transfer_encoding;
        assert!(
            dropped || stored.iter().any(|v| v.as_slice() == h.value(bytes)),
            "value of {name} in {shown:?}: {:?} not in {stored:?}",
            String::from_utf8_lossy(h.value(bytes))
        );
    }
    let facts = head.facts;
    assert_eq!(facts.transfer_encoding, classic.facts.transfer_encoding);
    if facts.transfer_encoding {
        let chunked = crate::common::classic::request::is_chunked_encoding_from_headers(
            &classic.header.headers,
        );
        assert_eq!(facts.chunked_final, chunked, "{shown:?}");
        assert_eq!(facts.content_length, None);
    } else {
        assert_eq!(
            facts.content_length,
            framing_length(&classic.header.headers).unwrap(),
            "{shown:?}"
        );
    }
    assert_eq!(facts.connection, keep_alive(classic), "{shown:?}");
    assert_eq!(facts.upgrade, classic.facts.upgrade);
    assert_eq!(
        facts.informational,
        classic.header.status.is_informational()
    );
    let options = options_of(classic).expect("a carried head has valid options");
    let ours: Vec<String> = head
        .nominated(bytes)
        .map(|o| String::from_utf8(o.to_vec()).unwrap())
        .collect();
    assert_eq!(ours, nominated(&options), "{shown:?}");
    for cut in 0..raw_len {
        let (partial, ..) = read(&raw[..cut]);
        assert_eq!(partial, HeadRead::Partial, "prefix {cut} of {shown:?}");
    }
    let mut extended = raw[..raw_len].to_vec();
    extended.extend_from_slice(b"HTTP/1.1 200 OK\r\n\x00\r\n\r\n\xff");
    let (again, again_head, again_canon) = read(&extended);
    assert_eq!(again, outcome, "{shown:?}");
    assert_eq!(again_canon, canon);
    assert_eq!(again_head.facts, head.facts);
    if canonical {
        let mut twice = Vec::new();
        assert_eq!(
            canonicalize_response(&canon, &mut twice),
            Canonical::Done {
                raw_len: canon.len()
            }
        );
        assert_eq!(twice, canon, "canonical form of {shown:?} is stable");
    }
}

fn check(raw: &[u8]) -> Oracle {
    let verdict = classify_with_classic(raw);
    let (outcome, ..) = read(raw);
    match (&verdict.oracle, outcome) {
        (Oracle::Partial, HeadRead::Partial) | (Oracle::Refused, HeadRead::Refused) => {}
        (Oracle::Head(_), HeadRead::Head { .. }) => {
            assert_reads_as_classic(raw, verdict.classic.as_ref().unwrap());
        }
        (oracle, outcome) => panic!(
            "classic {oracle:?} but read {outcome:?} for {:?}",
            String::from_utf8_lossy(raw)
        ),
    }
    verdict.oracle
}

fn canonical_of(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let Canonical::Done { raw_len } = canonicalize_response(raw, &mut out) else {
        panic!("not a head: {:?}", String::from_utf8_lossy(raw));
    };
    assert_eq!(raw_len, raw.len(), "{:?}", String::from_utf8_lossy(raw));
    out
}

#[test]
fn unusual_heads_are_written_in_the_canonical_shape() {
    for (raw, canonical) in [
        (
            &b"HTTP/1.1 200 OK\nContent-Length: 2\n\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n"[..],
        ),
        (
            b"HTTP/1.1 200\r\nA: b\r\n\r\n",
            b"HTTP/1.1 200 \r\nA: b\r\n\r\n",
        ),
        (b"HTTP/1.1 200\n\n", b"HTTP/1.1 200 \r\n\r\n"),
        (b"HTTP/1.1 200 K\xe9\r\n\r\n", b"HTTP/1.1 200 \r\n\r\n"),
        (
            b"HTTP/1.0 200  Odd \t\r\n\r\n",
            b"HTTP/1.0 200  Odd \t\r\n\r\n",
        ),
        (b"\r\n\nHTTP/1.1 200 OK\r\n\r\n", b"HTTP/1.1 200 OK\r\n\r\n"),
        (
            b"HTTP/1.1 200 OK\r\nX-A : a\r\nY\t:\tb\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nX-A: a\r\nY: b\r\n\r\n",
        ),
        (
            b"HTTP/1.1 200 OK\r\nX-A: one\r\n  two\r\n\tthree \r\n\r\n",
            b"HTTP/1.1 200 OK\r\nX-A: one two three\r\n\r\n",
        ),
        (
            b"HTTP/1.1 200 OK\r\nX-A:\r\n folded\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nX-A: folded\r\n\r\n",
        ),
        (
            b"HTTP/1.1 200 OK\r\nX-A:\r\nX-B: b\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nX-A: \r\nX-B: b\r\n\r\n",
        ),
        (
            b"HTTP/1.1 200 OK\nX-A: a\r\nX-B: b\n\r\n",
            b"HTTP/1.1 200 OK\r\nX-A: a\r\nX-B: b\r\n\r\n",
        ),
        (
            b"HTTP/1.1 200 OK\r\nX-A: \xe9\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nX-A: \xe9\r\n\r\n",
        ),
    ] {
        assert_eq!(
            canonical_of(raw),
            canonical,
            "{:?}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn what_the_classic_parser_refuses_is_refused() {
    for raw in [
        &b"HTTP/1.2 200 OK\r\n\r\n"[..],
        b"HTTP/2.0 200 OK\r\n\r\n",
        b"http/1.1 200 OK\r\n\r\n",
        b"HTTP/1.1  200 OK\r\n\r\n",
        b"HTTP/1.1 20 OK\r\n\r\n",
        b"HTTP/1.1 099 OK\r\n\r\n",
        b"HTTP/1.1 200 O\rK\r\n\r\n",
        b"HTTP/1.1 200 O\x01K\r\n\r\n",
        b"HTTP/1.1 200\x01\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n A: b\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nA b: c\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n: a\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nbroken\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nA: a\x01b\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nA: a\x00b\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nA: a\x7fb\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nA: a\rb\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nA: a\r\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n\r \n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n 2\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2 2\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length:\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999\r\n\r\n",
    ] {
        assert_eq!(
            check(raw),
            Oracle::Refused,
            "{:?}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn what_the_classic_parser_reads_is_read_the_same() {
    for raw in [
        &b"HTTP/1.1 200 OK\nContent-Length: 2\n\nok"[..],
        b"HTTP/1.1 200\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 \r\n\r\n",
        b"HTTP/1.1 200 K\xe9\r\n\r\n",
        b"\r\nHTTP/1.1 200 OK\r\n\r\n",
        b"HTTP/1.1 600 X\r\nContent-Length: 2\r\n\r\n",
        b"HTTP/1.1 999 X\r\n\r\n",
        b"HTTP/1.1 103 Early Hints\r\nLink: </a>; rel=preload\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nX-A: one\r\n  folded\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nX-A : a\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length : 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2, 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: abc\r\nTransfer-Encoding: chunked\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n ,gzip\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nUpgrade: h2,h2c\r\nConnection: Upgrade, close\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nUpgrade: h2c\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nConnection: X-Foo\r\nX-Foo: bar\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: x-a\r\nConnection: X-B, close\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: keep-alive, close\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: close,\r\n x-foo\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: a, b, c, d\r\n\r\n",
    ] {
        assert!(matches!(check(raw), Oracle::Head(_)), "{:?}", String::from_utf8_lossy(raw));
    }
}

#[test]
fn what_this_crate_does_not_carry_is_refused_where_the_classic_parser_reads_it() {
    let many = "X-a: v\r\n".repeat(MAX_RESPONSE_HEADERS + 1);
    let too_many = format!("HTTP/1.1 200 OK\r\n{many}\r\n");
    let long_name = format!(
        "HTTP/1.1 200 OK\r\n{}: v\r\n\r\n",
        "n".repeat(MAX_NAME_LEN + 1)
    );
    for raw in [
        too_many.as_bytes(),
        long_name.as_bytes(),
        b"HTTP/1.1 200 OK\r\nConnection: a, b, c, d, e\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: Content-Length\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: transfer-encoding\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: Host\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: two words\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nConnection: x\xe9\r\n\r\n",
    ] {
        let (outcome, ..) = read(raw);
        assert_eq!(
            outcome,
            HeadRead::Refused,
            "{:?}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn the_clean_head_is_read_in_place_without_a_rewrite() {
    let raw = crlf("HTTP/1.1 200 OK\nContent-Length: 2\nConnection: keep-alive\n\nok");
    let (outcome, head, canonical) = read(&raw);
    assert_eq!(
        outcome,
        HeadRead::Head {
            canonical: false,
            raw_len: raw.len() - 2
        }
    );
    assert_eq!(canonical, b"");
    assert_eq!(head.facts.connection, Connection::KeepAlive);
}

#[test]
fn the_lenient_scan_reads_the_framing_and_connection_rules_of_the_classic_client() {
    let mut head = ResponseHead::new();
    let buf = crlf(
        "HTTP/1.1 600 X\nContent-Length: 7\nContent-Length: 7, 7\nConnection: Upgrade, X-Foo, close\nX-Foo: 1\n\n",
    );
    assert_eq!(scan_response(&buf, &mut head), ScanResponse::Slow);
    assert_eq!(scan_response_lenient(&buf, &mut head), ScanResponse::Head);
    assert_eq!(head.status, 600);
    assert_eq!(head.facts.content_length, Some(7));
    assert_eq!(head.facts.connection, Connection::Close);
    assert!(head.find(Class::Connection).is_some());
    let names: Vec<&[u8]> = head.nominated(&buf).collect();
    assert_eq!(names, [&b"X-Foo"[..]]);

    let keep = crlf("HTTP/1.1 200 OK\nConnection: keep-alive\n\n");
    assert_eq!(scan_response_lenient(&keep, &mut head), ScanResponse::Head);
    assert_eq!(head.facts.connection, Connection::KeepAlive);
    let list = crlf("HTTP/1.1 200 OK\nConnection: keep-alive, x-a\n\n");
    assert_eq!(scan_response_lenient(&list, &mut head), ScanResponse::Head);
    assert_eq!(head.facts.connection, Connection::Absent);
    assert_eq!(head.nominated(&list).count(), 1);
}

#[test]
fn the_strict_scan_is_unchanged_by_the_lenient_one() {
    let mut head = ResponseHead::new();
    for text in [
        "HTTP/1.1 600 X\n\n",
        "HTTP/1.1 200 OK\nContent-Length: 2\nContent-Length: 2\n\n",
        "HTTP/1.1 200 OK\nConnection: x-a\n\n",
        "HTTP/1.1 200 OK\nContent-Length: abc\nTransfer-Encoding: chunked\n\n",
    ] {
        assert_eq!(
            scan_response(&crlf(text), &mut head),
            ScanResponse::Slow,
            "{text:?}"
        );
    }
}

const SEEDS: [&str; 10] = [
    "HTTP/1.1 200 OK\nContent-Length: 5\nConnection: keep-alive\nKeep-Alive: timeout=5\nX-A:  padded \t\n\nhello",
    "HTTP/1.1 200 OK\nTransfer-Encoding: gzip, chunked\nContent-Length: 4\nSet-Cookie: a=1\nSet-Cookie: b=2\n\n",
    "HTTP/1.0 404 Not Found\nVary: a\nX-B: 1\nVary: b\nUpgrade: h2c\nConnection: Upgrade, X-B\n\n",
    "HTTP/1.1 103 Early Hints\nLink: </a.css>; rel=preload\n\n",
    "HTTP/1.1 200 OK\nX-A: one\n  two\n\tthree\nContent-Length: 2\n\nok",
    "HTTP/1.1 200\nX-A : a\nContent-Length : 2, 2\n\n",
    "HTTP/1.1 200 K\u{e9}\nA:\nB: \nC:\t\nContent-Length: 0\n\n",
    "HTTP/1.1 600 X\nConnection: a, B,c ,, close\nA: 1\nB: 2\n\n",
    "HTTP/1.1 200 OK\nContent-Length: 3\nContent-Length: 3\nTransfer-Encoding: chunked\n\n",
    "HTTP/1.1 204 \nX-Request-Id: u\nx-request-id: v\nProxy-Connection: x\nX_A: 1\n\n",
];

fn lenient_variants(seed: &[u8]) -> Vec<Vec<u8>> {
    let lf = seed.windows(2).fold(Vec::new(), |mut out, pair| {
        if pair != b"\r\n" {
            out.push(pair[0]);
        }
        out
    });
    let mut variants = vec![seed.to_vec(), lf];
    let mut folded = seed.to_vec();
    if let Some(at) = folded.windows(2).position(|w| w == b": ") {
        folded.splice(at + 1..at + 2, *b"\r\n ");
    }
    variants.push(folded);
    let mut spaced = seed.to_vec();
    if let Some(at) = spaced.windows(1).position(|w| w == b":") {
        spaced.insert(at, b' ');
    }
    variants.push(spaced);
    variants
}

#[test]
fn mutated_heads_are_read_as_the_classic_parser_reads_them() {
    let seeds: Vec<Vec<u8>> = SEEDS
        .iter()
        .flat_map(|s| lenient_variants(&crlf(s)))
        .collect();
    let mut rng = Rng(0x5EED_1E71_E417_0001);
    let (mut heads, mut refused) = (0, 0);
    let rounds = std::env::var("H1_HEAD_MUTATION_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse::<usize>().ok())
        .unwrap_or(60_000);
    for _ in 0..rounds {
        let mut buf = rng.pick(&seeds).clone();
        mutate(&mut rng, &mut buf, MUTATION_ALPHABET);
        match check(&buf) {
            Oracle::Head(_) => heads += 1,
            Oracle::Refused => refused += 1,
            Oracle::Partial => {}
        }
    }
    assert!(
        heads > 5_000 && refused > 5_000,
        "{heads} heads, {refused} refused"
    );
}

#[test]
fn every_seed_variant_is_read_as_the_classic_parser_reads_it() {
    for seed in SEEDS {
        for variant in lenient_variants(&crlf(seed)) {
            check(&variant);
        }
    }
}

#[test]
fn every_byte_in_every_position_is_read_as_the_classic_parser_reads_it() {
    for byte in 0..=u8::MAX {
        for (before, after) in [
            (&b"HTTP/1.1 200 OK\r\nA"[..], &b": v\r\n\r\n"[..]),
            (b"HTTP/1.1 200 OK\r\n", b"A: v\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nA:", b"v\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nA: v", b"w\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nA: v\r\n", b"w\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nA: v\r\n", b" w\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nA ", b": v\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nA: v\r", b"\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nA: v\r\n\r", b""),
            (b"HTTP/1.1 200", b"\r\n\r\n"),
            (b"HTTP/1.1 200 ", b"OK\r\n\r\n"),
            (b"HTTP/1.1 200 O", b"\r\n\r\n"),
            (b"HTTP/1.1", b"200 OK\r\n\r\n"),
            (b"HTTP/1.1 20", b"0 OK\r\n\r\n"),
            (b"HTTP/1.", b"1 200 OK\r\n\r\n"),
            (b"", b"HTTP/1.1 200 OK\r\n\r\n"),
            (b"\r\n", b"HTTP/1.1 200 OK\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nConnection: ", b"\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nConnection: close,", b"\r\n\r\n"),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 1", b"\r\n\r\n"),
        ] {
            let mut raw = before.to_vec();
            raw.push(byte);
            raw.extend_from_slice(after);
            check(&raw);
        }
    }
}
