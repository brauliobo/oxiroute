//! `write_request` against the classic write: `RequestHeader` edited through the real `HeaderMap` calls and
//! serialised by `header_to_h1_wire`.

mod common;

use common::classic_request::classic_request;
use common::classic_write::{Kind, OwnedEdit, lend, wire};
use common::{Rng, request_corpus};
use oxiroute_h1_head::{RequestHead, Scan, names, scan_request, write_request};

fn written(buf: &[u8], head: &RequestHead, edits: &[OwnedEdit]) -> Vec<u8> {
    let mut out = Vec::new();
    write_request(&mut out, buf, head, &lend(edits));
    out
}

fn edit(kind: Kind, name: &'static str, value: &str) -> OwnedEdit {
    OwnedEdit {
        kind,
        name,
        value: value.as_bytes().to_vec(),
    }
}

/// Writes with the lane and with the classic path and compares; returns the bytes.
fn assert_same(buf: &[u8], edits: &[OwnedEdit]) -> Vec<u8> {
    let mut head = RequestHead::new();
    assert_eq!(
        scan_request(buf, &mut head),
        Scan::Head,
        "{:?}",
        String::from_utf8_lossy(buf)
    );
    let classic = classic_request(buf).unwrap();
    let removes_length = classic.facts.transfer_encoding
        && head
            .headers()
            .iter()
            .any(|h| h.name(buf).eq_ignore_ascii_case(b"content-length"));
    let mut all = Vec::new();
    if removes_length {
        all.push(OwnedEdit {
            kind: Kind::Remove,
            name: "content-length",
            value: Vec::new(),
        });
    }
    all.extend_from_slice(edits);
    let ours = written(buf, &head, &all);
    let theirs = wire(classic_request(buf).unwrap(), edits);
    assert_eq!(
        String::from_utf8_lossy(&ours),
        String::from_utf8_lossy(&theirs),
        "head {:?} edits {edits:?}",
        String::from_utf8_lossy(buf)
    );
    ours
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn names_are_titled_like_pingora_http_and_others_lowercased() {
    let out = assert_same(
        b"GET /a HTTP/1.1\r\nhost:a\r\nUSER-AGENT: x\r\nConnection:keep-alive\r\nCONTENT-length: 0\r\nX-Mixed-Case: y\r\ncache-control: z\r\n\r\n",
        &[],
    );
    assert_eq!(
        text(&out),
        "GET /a HTTP/1.1\r\nHost: a\r\nuser-agent: x\r\nConnection: keep-alive\r\nContent-Length: 0\r\nx-mixed-case: y\r\nCache-Control: z\r\n\r\n"
    );
}

#[test]
fn values_are_trimmed_and_copied_verbatim() {
    let out = assert_same(b"GET / HTTP/1.1\r\nHost:   a b \t\r\nX:\tv\t w\t\r\nEmpty:\r\nBlank:  \r\nHigh: caf\xc3\xa9\r\n\r\n", &[]);
    assert_eq!(text(&out[..out.len() - 4]).lines().count(), 6);
    assert!(out.windows(12).any(|w| w == b"high: caf\xc3\xa9\r"));
}

#[test]
fn duplicates_are_grouped_after_their_first_occurrence() {
    let out = assert_same(b"GET / HTTP/1.1\r\nHost: a\r\nX-A: 1\r\nX-B: 1\r\nx-a: 2\r\nX-C: 1\r\nX-B: 2\r\nX-A: 3\r\n\r\n", &[]);
    assert_eq!(
        text(&out),
        "GET / HTTP/1.1\r\nHost: a\r\nx-a: 1\r\nx-a: 2\r\nx-a: 3\r\nx-b: 1\r\nx-b: 2\r\nx-c: 1\r\n\r\n"
    );
}

#[test]
fn remove_moves_the_last_entry_into_the_hole() {
    let head = b"GET / HTTP/1.1\r\nA: 1\r\nB: 2\r\nHost: h\r\nC: 3\r\nD: 4\r\n\r\n";
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Remove, "a", "")])),
        "GET / HTTP/1.1\r\nd: 4\r\nb: 2\r\nHost: h\r\nc: 3\r\n\r\n"
    );
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Remove, "host", "")])),
        "GET / HTTP/1.1\r\na: 1\r\nb: 2\r\nd: 4\r\nc: 3\r\n\r\n"
    );
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Remove, "d", "")])),
        "GET / HTTP/1.1\r\na: 1\r\nb: 2\r\nHost: h\r\nc: 3\r\n\r\n"
    );
    assert_eq!(
        text(&assert_same(
            head,
            &[edit(Kind::Remove, "a", ""), edit(Kind::Remove, "d", "")]
        )),
        "GET / HTTP/1.1\r\nc: 3\r\nb: 2\r\nHost: h\r\n\r\n"
    );
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Remove, "zzz", "")])),
        text(&assert_same(head, &[]))
    );
}

#[test]
fn remove_takes_all_values_and_the_last_entry_with_its_extras_moves() {
    let head = b"GET / HTTP/1.1\r\nHost: h\r\nA: 1\r\nB: 1\r\nC: 1\r\nC: 2\r\nA: 2\r\n\r\n";
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Remove, "a", "")])),
        "GET / HTTP/1.1\r\nHost: h\r\nc: 1\r\nc: 2\r\nb: 1\r\n\r\n"
    );
}

#[test]
fn set_keeps_the_position_drops_extras_and_appends_when_absent() {
    let head = b"GET / HTTP/1.1\r\nHost: h\r\nA: 1\r\nB: 1\r\nA: 2\r\nA: 3\r\n\r\n";
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Set, "a", "new")])),
        "GET / HTTP/1.1\r\nHost: h\r\na: new\r\nb: 1\r\n\r\n"
    );
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Set, "x-request-id", "7")])),
        "GET / HTTP/1.1\r\nHost: h\r\na: 1\r\na: 2\r\na: 3\r\nb: 1\r\nx-request-id: 7\r\n\r\n"
    );
    assert_eq!(
        text(&assert_same(
            head,
            &[edit(Kind::Remove, "a", ""), edit(Kind::Set, "a", "z")]
        )),
        "GET / HTTP/1.1\r\nHost: h\r\nb: 1\r\na: z\r\n\r\n"
    );
    assert_eq!(
        text(&assert_same(head, &[edit(Kind::Set, "host", "other")])),
        "GET / HTTP/1.1\r\nHost: other\r\na: 1\r\na: 2\r\na: 3\r\nb: 1\r\n\r\n"
    );
}

#[test]
fn add_goes_after_the_last_value_of_the_name_or_at_the_end() {
    let head = b"GET / HTTP/1.1\r\nHost: h\r\nA: 1\r\nB: 1\r\nA: 2\r\n\r\n";
    assert_eq!(
        text(&assert_same(
            head,
            &[
                edit(Kind::Add, "a", "3"),
                edit(Kind::Add, "c", "1"),
                edit(Kind::Add, "c", "2")
            ]
        )),
        "GET / HTTP/1.1\r\nHost: h\r\na: 1\r\na: 2\r\na: 3\r\nb: 1\r\nc: 1\r\nc: 2\r\n\r\n"
    );
}

#[test]
fn only_header_removed_leaves_a_bare_request_line() {
    let out = assert_same(
        b"GET / HTTP/1.1\r\nHost: h\r\n\r\n",
        &[edit(Kind::Remove, "host", "")],
    );
    assert_eq!(text(&out), "GET / HTTP/1.1\r\n\r\n");
}

#[test]
fn a_transfer_encoding_head_drops_its_length_like_classic() {
    assert_same(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\nX: y\r\n\r\n", &[]);
}

#[test]
fn the_corpus_writes_like_classic_with_and_without_edits() {
    let edits = [
        edit(Kind::Remove, "connection", ""),
        edit(Kind::Remove, "keep-alive", ""),
        edit(Kind::Remove, "te", ""),
        edit(Kind::Set, "x-request-id", "0123456789abcdef"),
        edit(Kind::Add, "x-forwarded-for", "192.0.2.1"),
    ];
    let mut head = RequestHead::new();
    let mut count = 0;
    for (_, bytes, _) in request_corpus::corpus() {
        if scan_request(&bytes, &mut head) == Scan::Head {
            assert_same(&bytes, &[]);
            assert_same(&bytes, &edits);
            count += 1;
        }
    }
    assert!(count > 50);
}

const NAMES: [&str; 16] = [
    "Host",
    "X-A",
    "x-a",
    "X-B",
    "Cookie",
    "COOKIE",
    "Connection",
    "keep-alive",
    "TE",
    "X-Request-Id",
    "Accept",
    "User-Agent",
    "Set-Cookie",
    "content-type",
    "X_under",
    "Upgrade",
];
const EDIT_NAMES: [&str; 14] = [
    "host",
    "x-a",
    "x-b",
    "cookie",
    "connection",
    "keep-alive",
    "te",
    "x-request-id",
    "accept",
    "user-agent",
    "set-cookie",
    "x-new",
    "upgrade",
    "x_under",
];
const VALUES: [&str; 7] = ["", "v", "a b", "text/html; q=1", "1", "keep-alive", "x\ty"];
const SEPARATORS: [&str; 5] = [": ", ":", ":  ", ":\t", ": "];
const TAILS: [&str; 4] = ["", "", " ", "\t "];

fn random_head(rng: &mut Rng) -> Vec<u8> {
    let mut lines: Vec<String> = Vec::new();
    for _ in 0..rng.below(14) {
        let name = rng.pick(&NAMES);
        if name.eq_ignore_ascii_case("host") {
            continue;
        }
        lines.push(format!(
            "{name}{}{}{}",
            rng.pick(&SEPARATORS),
            rng.pick(&VALUES),
            rng.pick(&TAILS)
        ));
    }
    let at = rng.below(lines.len() + 1);
    lines.insert(
        at,
        format!(
            "{}{}h",
            rng.pick(&["Host", "host", "HOST"]),
            rng.pick(&SEPARATORS)
        ),
    );
    format!(
        "GET /p?q={} HTTP/1.1\r\n{}\r\n\r\n",
        rng.below(10),
        lines.join("\r\n")
    )
    .into_bytes()
}

fn random_edits(rng: &mut Rng) -> Vec<OwnedEdit> {
    (0..rng.below(11))
        .map(|_| {
            let kind = *rng.pick(&[Kind::Remove, Kind::Remove, Kind::Set, Kind::Add]);
            OwnedEdit {
                kind,
                name: rng.pick(&EDIT_NAMES),
                value: rng.pick(&VALUES).as_bytes().to_vec(),
            }
        })
        .collect()
}

#[test]
fn random_heads_and_edit_sequences_write_like_classic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut head = RequestHead::new();
    let (mut heads, mut with_duplicates) = (0, 0);
    for _ in 0..200_000 {
        let buf = random_head(&mut rng);
        if scan_request(&buf, &mut head) != Scan::Head {
            continue;
        }
        heads += 1;
        with_duplicates += usize::from(head.facts.duplicate_names);
        assert_same(&buf, &random_edits(&mut rng));
    }
    eprintln!("{heads} heads written, {with_duplicates} with duplicate names");
    assert!(heads > 150_000 && with_duplicates > 30_000);
}

#[test]
fn writing_appends_to_the_buffer_it_is_given() {
    let buf = b"GET /a HTTP/1.1\r\nHost: h\r\n\r\n";
    let mut head = RequestHead::new();
    assert_eq!(scan_request(buf, &mut head), Scan::Head);
    let mut out = b"prefix|".to_vec();
    let mut edits = oxiroute_h1_head::Edits::new();
    edits
        .set(
            names::X_REQUEST_ID,
            oxiroute_h1_head::Value::new(b"1").unwrap(),
        )
        .unwrap();
    write_request(&mut out, buf, &head, &edits);
    assert_eq!(
        text(&out),
        "prefix|GET /a HTTP/1.1\r\nHost: h\r\nx-request-id: 1\r\n\r\n"
    );
}
