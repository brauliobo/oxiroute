//! `write_response` against the vendored serializer (`http_resp_header_to_buf`) over a `ResponseHeader` edited through
//! the real `HeaderMap` calls, and `stamp_request_id` against `stamp_response`.

mod common;

use common::Rng;
use common::classic_response::classic_response;
use common::classic_write::{Kind, OwnedEdit, apply, lend, serialize_response, stamp_response};
use common::request_corpus::crlf;
use common::response_corpus::{Verdict, corpus};
use http::header::HeaderValue;
use oxiroute_h1_head::{
    Automatic, Edits, ResponseHead, ScanResponse, Value, scan_response, write_response,
};

const DATE: &str = "Mon, 09 Nov 2026 20:00:00 GMT";

fn edit(kind: Kind, name: &'static str, value: &str) -> OwnedEdit {
    OwnedEdit {
        kind,
        name,
        value: value.as_bytes().to_vec(),
    }
}

fn written(
    buf: &[u8],
    head: &ResponseHead,
    edits: &Edits<'_>,
    minor: u8,
    auto: Option<&str>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let automatic = auto.map(|connection| Automatic {
        date: DATE.as_bytes(),
        connection: connection.as_bytes(),
    });
    write_response(&mut out, buf, head, edits, minor, automatic);
    out
}

/// What the classic path does before the lane's edits: a `Transfer-Encoding` head loses its `Content-Length`.
fn classic_prelude(buf: &[u8], head: &ResponseHead) -> Vec<OwnedEdit> {
    let drops_length = head.facts.transfer_encoding
        && head
            .headers()
            .iter()
            .any(|h| h.name(buf).eq_ignore_ascii_case(b"content-length"));
    drops_length
        .then(|| edit(Kind::Remove, "content-length", ""))
        .into_iter()
        .collect()
}

/// Writes with the lane and with the classic serializer for the four combinations of version and automatic headers;
/// returns the version 1.1 output without automatic headers.
fn assert_same(buf: &[u8], edits: &[OwnedEdit]) -> Vec<u8> {
    let mut head = ResponseHead::new();
    assert_eq!(
        scan_response(buf, &mut head),
        ScanResponse::Head,
        "{:?}",
        String::from_utf8_lossy(buf)
    );
    let mut all = classic_prelude(buf, &head);
    all.extend_from_slice(edits);
    let mut first = Vec::new();
    for (minor, auto) in [
        (1, None),
        (1, Some("keep-alive")),
        (0, Some("close")),
        (0, None),
    ] {
        let mut classic = classic_response(buf).unwrap();
        apply(&mut classic.header.headers, edits);
        let theirs = serialize_response(&classic.header, minor, auto.map(|c| (DATE, c)));
        let ours = written(buf, &head, &lend(&all), minor, auto);
        assert_eq!(
            String::from_utf8_lossy(&ours),
            String::from_utf8_lossy(&theirs),
            "head {:?} edits {edits:?} minor {minor} auto {auto:?}",
            String::from_utf8_lossy(buf)
        );
        if first.is_empty() {
            first = ours;
        }
    }
    first
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn date_and_connection_move_to_the_end_wherever_they_were() {
    let buf = crlf(
        "HTTP/1.1 200 OK\nConnection: close\nServer: s\nDate: old\nContent-Length: 2\nX-A: 1\n\n",
    );
    let mut head = ResponseHead::new();
    assert_eq!(scan_response(&buf, &mut head), ScanResponse::Head);
    let out = written(&buf, &head, &Edits::new(), 1, Some("keep-alive"));
    assert_eq!(
        text(&out),
        format!(
            "HTTP/1.1 200 OK\r\nServer: s\r\nContent-Length: 2\r\nx-a: 1\r\nDate: {DATE}\r\nConnection: keep-alive\r\n\r\n"
        )
    );
    assert_same(&buf, &[]);
    let out = written(&buf, &head, &Edits::new(), 1, None);
    assert_eq!(
        text(&out),
        "HTTP/1.1 200 OK\r\nConnection: close\r\nServer: s\r\nDate: old\r\nContent-Length: 2\r\nx-a: 1\r\n\r\n"
    );
}

#[test]
fn edits_that_add_date_or_connection_are_left_out_with_automatic_headers() {
    let buf = crlf("HTTP/1.1 200 OK\nServer: s\n\n");
    assert_same(
        &buf,
        &[
            edit(Kind::Set, "date", "x"),
            edit(Kind::Add, "connection", "y"),
            edit(Kind::Set, "x-a", "1"),
        ],
    );
}

#[test]
fn hop_by_hop_removals_reorder_like_the_map() {
    let buf = crlf(
        "HTTP/1.1 200 OK\nConnection: close\nKeep-Alive: 5\nServer: s\nTE: x\nContent-Type: t\nTrailer: y\nX-A: 1\nUpgrade: h2c\n\n",
    );
    let hops = [
        "connection",
        "keep-alive",
        "te",
        "trailer",
        "upgrade",
        "proxy-connection",
        "transfer-encoding",
    ];
    let removals: Vec<OwnedEdit> = hops.iter().map(|n| edit(Kind::Remove, n, "")).collect();
    assert_same(&buf, &removals);
    assert_same(&buf, &removals[2..]);
    let mut reversed = removals.clone();
    reversed.reverse();
    assert_same(&buf, &reversed);
}

#[test]
fn status_line_carries_the_reason_as_sent() {
    for (head, line) in [
        ("HTTP/1.1 200 OK\n\n", "HTTP/1.1 200 OK"),
        ("HTTP/1.1 404 Nope \t.\n\n", "HTTP/1.1 404 Nope \t."),
        ("HTTP/1.1 200 \n\n", "HTTP/1.1 200 "),
        ("HTTP/1.1 299 Odd\n\n", "HTTP/1.1 299 Odd"),
        ("HTTP/1.1 404 OK\n\n", "HTTP/1.1 404 OK"),
    ] {
        let out = assert_same(&crlf(head), &[]);
        assert!(text(&out).starts_with(&format!("{line}\r\n")), "{line}");
    }
    let buf = crlf("HTTP/1.0 200 OK\n\n");
    let mut parsed = ResponseHead::new();
    assert_eq!(scan_response(&buf, &mut parsed), ScanResponse::Head);
    assert!(
        text(&written(&buf, &parsed, &Edits::new(), 1, None)).starts_with("HTTP/1.1 200 OK\r\n")
    );
}

#[test]
fn request_id_stamping_matches_stamp_response() {
    let id = "0123456789abcdef";
    for upstream in [
        "HTTP/1.1 200 OK\nServer: s\n\n",
        "HTTP/1.1 200 OK\nX-Request-Id: 0123456789abcdef\nServer: s\n\n",
        "HTTP/1.1 200 OK\nServer: s\nX-Request-Id: up\nDate: d\n\n",
        "HTTP/1.1 200 OK\nX-Request-Id: up\nX-Request-Id: up2\nServer: s\nx-upstream-request-id: old\n\n",
        "HTTP/1.1 200 OK\nA: 1\nB: 2\nX-Request-Id: up\nC: 3\nD: 4\nX-Upstream-Request-Id: old\nX-Request-Id: id\n\n",
        "HTTP/1.1 200 OK\nX-Request-Id: up\n\n",
    ] {
        let buf = crlf(upstream);
        let mut head = ResponseHead::new();
        assert_eq!(scan_response(&buf, &mut head), ScanResponse::Head);
        let mut edits = Edits::new();
        edits
            .stamp_request_id(&buf, &head, Value::new(id.as_bytes()).unwrap())
            .unwrap();
        for auto in [None, Some("close")] {
            let mut classic = classic_response(&buf).unwrap();
            stamp_response(&mut classic.header, &HeaderValue::from_static(id));
            let theirs = serialize_response(&classic.header, 1, auto.map(|c| (DATE, c)));
            assert_eq!(
                text(&written(&buf, &head, &edits, 1, auto)),
                text(&theirs),
                "{upstream:?}"
            );
        }
    }
}

#[test]
fn the_corpus_writes_like_classic_with_and_without_edits() {
    let hop = [
        "connection",
        "keep-alive",
        "te",
        "trailer",
        "upgrade",
        "proxy-connection",
    ];
    let mut edits: Vec<OwnedEdit> = hop.iter().map(|n| edit(Kind::Remove, n, "")).collect();
    edits.push(edit(Kind::Set, "x-request-id", "0123456789abcdef"));
    let mut head = ResponseHead::new();
    let mut count = 0;
    for (_, bytes, verdict) in corpus() {
        if verdict == Verdict::Head && scan_response(&bytes, &mut head) == ScanResponse::Head {
            assert_same(&bytes, &[]);
            assert_same(&bytes, &edits);
            count += 1;
        }
    }
    assert!(count > 30);
}

const NAMES: [&str; 18] = [
    "Date",
    "Connection",
    "Server",
    "Content-Type",
    "content-length",
    "Set-Cookie",
    "set-cookie",
    "X-Request-Id",
    "X-Upstream-Request-Id",
    "Keep-Alive",
    "TE",
    "Vary",
    "X-A",
    "x-a",
    "Cache-Control",
    "Age",
    "X_under",
    "Accept-Ranges",
];
const EDIT_NAMES: [&str; 17] = [
    "date",
    "connection",
    "server",
    "content-type",
    "set-cookie",
    "x-request-id",
    "x-upstream-request-id",
    "keep-alive",
    "te",
    "vary",
    "x-a",
    "cache-control",
    "age",
    "upgrade",
    "proxy-connection",
    "trailer",
    "x-new",
];
const VALUES: [&str; 7] = ["", "v", "a b", "text/html; q=1", "1", "close", "x\ty"];
const SEPARATORS: [&str; 4] = [": ", ":", ":  ", ":\t"];
const TAILS: [&str; 4] = ["", "", " ", "\t "];
const STATUS_LINES: [&str; 6] = [
    "200 OK",
    "404 Not Found",
    "204 ",
    "299 Odd",
    "304 Not Modified",
    "500 OK",
];

fn random_head(rng: &mut Rng) -> Vec<u8> {
    let lines: Vec<String> = (0..rng.below(14))
        .map(|_| {
            let (value, tail) = (rng.pick(&VALUES), rng.pick(&TAILS));
            format!("{}{}{value}{tail}", rng.pick(&NAMES), rng.pick(&SEPARATORS))
        })
        .collect();
    let minor = rng.below(2);
    format!(
        "HTTP/1.{minor} {}\r\n{}\r\n\r\n",
        rng.pick(&STATUS_LINES),
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
    let mut rng = Rng(0x0F1E_2D3C_4B5A_6978);
    let mut head = ResponseHead::new();
    let (mut heads, mut with_duplicates) = (0, 0);
    for _ in 0..100_000 {
        let buf = random_head(&mut rng);
        if scan_response(&buf, &mut head) != ScanResponse::Head {
            continue;
        }
        heads += 1;
        with_duplicates += usize::from(head.facts.duplicate_names);
        assert_same(&buf, &random_edits(&mut rng));
    }
    eprintln!("{heads} heads written, {with_duplicates} with duplicate names");
    assert!(heads > 30_000 && with_duplicates > 10_000);
}
