#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! What the epoll data plane does with an origin's response, against what the tokio plane does with the same bytes:
//! the proxy is the real binary in front of a raw TCP origin that answers with a scripted byte string, the client is a
//! raw socket, and the bytes the client receives are compared between the two planes (the date and the request id
//! aside). Every case also states what the client must see, so two planes that agree on a failure cannot pass.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use oxiroute_config::{ConfigDraft, HttpDataPlane};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};

use h1_wire::{Origin, STEP, Step, Wire, find};

const GET: &[u8] = b"GET /r HTTP/1.1\r\nHost: example.test\r\n\r\n";
const SERVED_ON_EPOLL: &str = "served by the epoll data plane";
const SETTLE: Duration = Duration::from_millis(300);
const BAD_GATEWAY: &str = "HTTP/1.1 502 Bad Gateway\r\n";

struct Case {
    name: &'static str,
    reply: &'static [u8],
    closes: bool,
    starts: &'static str,
    contains: &'static [&'static str],
    absent: &'static [&'static str],
}

fn case(name: &'static str, reply: &'static [u8], starts: &'static str) -> Case {
    Case {
        name,
        reply,
        closes: false,
        starts,
        contains: &[],
        absent: &[],
    }
}

impl Case {
    const fn closing(mut self) -> Self {
        self.closes = true;
        self
    }

    const fn with(mut self, contains: &'static [&'static str]) -> Self {
        self.contains = contains;
        self
    }

    const fn without(mut self, absent: &'static [&'static str]) -> Self {
        self.absent = absent;
        self
    }
}

fn leaked(bytes: Vec<u8>) -> &'static [u8] {
    Box::leak(bytes.into_boxed_slice())
}

fn head_of(headers: usize, size: usize) -> &'static [u8] {
    let mut head = b"HTTP/1.1 200 OK\r\n".to_vec();
    for index in 0..headers {
        head.extend_from_slice(format!("X-H{index}: {}\r\n", "v".repeat(size)).as_bytes());
    }
    head.extend_from_slice(b"Content-Length: 2\r\n\r\nok");
    leaked(head)
}

fn logged(plane: HttpDataPlane) -> impl FnOnce(&mut ConfigDraft) {
    move |config| {
        config.log_filter = Some("oxiroute::epoll=debug".into());
        config.http_data_plane = Some(plane);
    }
}

fn normalized(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        let end = find(rest, b"\r\n").map_or(rest.len(), |at| at + 2);
        let (line, tail) = rest.split_at(end);
        let lower = line.to_ascii_lowercase();
        if lower.starts_with(b"date: ") {
            out.extend_from_slice(b"Date: X\r\n");
        } else if lower.starts_with(b"x-request-id: ") {
            out.extend_from_slice(b"x-request-id: X\r\n");
        } else {
            out.extend_from_slice(line);
        }
        rest = tail;
    }
    out
}

async fn answer_of(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0_u8; 65_536];
    let mut quiet = STEP;
    let end = loop {
        match timeout(quiet, stream.read(&mut buffer)).await {
            Ok(Ok(0)) => break &b"<EOF>"[..],
            Ok(Ok(count)) => {
                bytes.extend_from_slice(&buffer[..count]);
                quiet = SETTLE;
            }
            Ok(Err(_)) => break b"<RESET>",
            Err(_) => break b"<OPEN>",
        }
    };
    bytes.extend_from_slice(end);
    normalized(&bytes)
}

async fn answer(proxy: SocketAddr) -> Vec<u8> {
    let mut stream = TcpStream::connect(proxy).await.expect("connect");
    stream.write_all(GET).await.expect("write request");
    answer_of(&mut stream).await
}

async fn through(plane: HttpDataPlane, cases: &[Case]) -> (Vec<Vec<u8>>, usize) {
    let steps: Vec<Step> = cases
        .iter()
        .map(|case| {
            if case.closes {
                Step::ReplyThenClose(case.reply)
            } else {
                Step::Reply(case.reply)
            }
        })
        .collect();
    let origin = Origin::scripted(Box::leak(steps.into_boxed_slice())).await;
    let wire = Wire::over(origin, logged(plane)).await;
    let mut answers = Vec::new();
    for _ in cases {
        answers.push(answer(wire.proxy).await);
    }
    let output = wire.server.shutdown_output();
    let served = output
        .lines()
        .filter(|line| line.contains(SERVED_ON_EPOLL))
        .count();
    (answers, served)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn clipped(text: &str) -> String {
    text.chars().take(600).collect()
}

async fn agree(cases: &[Case]) {
    let (theirs, _) = through(HttpDataPlane::Tokio, cases).await;
    let (ours, served) = through(HttpDataPlane::Epoll, cases).await;
    if oxiroute_epoll_h1::supported() {
        assert_eq!(served, cases.len(), "requests the epoll plane served");
    }
    for ((case, theirs), ours) in cases.iter().zip(&theirs).zip(&ours) {
        let shown = text(ours);
        assert!(
            ours == theirs,
            "{}: the planes answer differently\n  tokio: {:?}\n  epoll: {:?}",
            case.name,
            clipped(&text(theirs)),
            clipped(&shown),
        );
        assert!(
            shown.starts_with(case.starts),
            "{}: expected a start of {:?}, got {:?}",
            case.name,
            case.starts,
            clipped(&shown)
        );
        let lower = shown.to_ascii_lowercase();
        for needle in case.contains {
            assert!(
                lower.contains(&needle.to_ascii_lowercase()),
                "{}: {needle:?} missing from {:?}",
                case.name,
                clipped(&shown)
            );
        }
        for needle in case.absent {
            assert!(
                !lower.contains(&needle.to_ascii_lowercase()),
                "{}: {needle:?} present in {:?}",
                case.name,
                clipped(&shown)
            );
        }
    }
}

#[tokio::test]
async fn heads_the_classic_parser_reads_are_forwarded_like_on_the_tokio_plane() {
    agree(&[
        case(
            "obs-fold",
            b"HTTP/1.1 200 OK\r\nX-A: one\r\n  folded\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-a: one folded\r\n", "\r\n\r\nok<open>"]),
        case(
            "obs-fold-over-two-lines",
            b"HTTP/1.1 200 OK\r\nX-A: one\r\n\tfolded\r\n two\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-a: one folded two\r\n"]),
        case(
            "obs-fold-after-an-empty-value",
            b"HTTP/1.1 200 OK\r\nX-A:\r\n folded\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-a: folded\r\n"]),
        case(
            "bare-lf",
            b"HTTP/1.1 200 OK\nContent-Length: 2\nX-A: b\n\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-a: b\r\n", "content-length: 2\r\n"]),
        case(
            "bare-lf-mixed-with-crlf",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\nX-A: b\r\n\nok",
            "HTTP/1.1 200 OK\r\n",
        ),
        case(
            "status-line-without-a-reason-phrase",
            b"HTTP/1.1 200\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 \r\n",
        ),
        case(
            "status-line-with-an-empty-reason-phrase",
            b"HTTP/1.1 200 \r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 \r\n",
        ),
        case(
            "reason-phrase-above-ascii",
            b"HTTP/1.1 200 K\xe9\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 \r\n",
        ),
        case(
            "custom-reason-phrase",
            b"HTTP/1.1 200 Fine\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 Fine\r\n",
        ),
        case(
            "empty-line-before-the-status-line",
            b"\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        ),
        case(
            "whitespace-before-the-colon",
            b"HTTP/1.1 200 OK\r\nX-A : a\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-a: a\r\n"]),
        case(
            "whitespace-before-the-colon-of-the-length",
            b"HTTP/1.1 200 OK\r\nContent-Length : 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["content-length: 2\r\n", "\r\n\r\nok<open>"]),
        case(
            "empty-header-value",
            b"HTTP/1.1 200 OK\r\nX-A:\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        ),
        case(
            "bytes-above-ascii-in-a-value",
            b"HTTP/1.1 200 OK\r\nX-A: a\xe9b\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        ),
    ])
    .await;
}

#[tokio::test]
async fn hop_by_hop_fields_of_a_final_response_are_removed_like_on_the_tokio_plane() {
    agree(&[
        case(
            "apache-advertising-h2c",
            b"HTTP/1.1 200 OK\r\nUpgrade: h2,h2c\r\nConnection: Upgrade, close\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["connection: keep-alive\r\n"])
        .without(&["h2c", "upgrade"]),
        case(
            "upgrade-without-connection",
            b"HTTP/1.1 200 OK\r\nUpgrade: h2c\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .without(&["h2c", "upgrade"]),
        case(
            "connection-names-a-header",
            b"HTTP/1.1 200 OK\r\nConnection: X-Foo\r\nX-Foo: bar\r\nX-Keep: 1\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-keep: 1\r\n"])
        .without(&["x-foo", "bar"]),
        case(
            "connection-names-headers-in-two-lines",
            b"HTTP/1.1 200 OK\r\nConnection: x-a\r\nX-A: 1\r\nX-K: k\r\nConnection: X-B, close\r\nX-B: 2\r\nX-L: l\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-k: k\r\n", "x-l: l\r\n"])
        .without(&["x-a", "x-b"]),
        case(
            "connection-names-the-last-headers",
            b"HTTP/1.1 200 OK\r\nA: a\r\nB: b\r\nC: c\r\nD: d\r\nConnection: a, b\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["c: c\r\n", "d: d\r\n"])
        .without(&["a: a", "b: b"]),
        case(
            "keep-alive-and-close-in-one-line",
            b"HTTP/1.1 200 OK\r\nConnection: keep-alive, close\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["connection: keep-alive\r\n"]),
        case(
            "connection-folded-over-two-lines",
            b"HTTP/1.1 200 OK\r\nConnection: close,\r\n x-foo\r\nX-Foo: 1\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .without(&["x-foo"]),
        case(
            "keep-alive-header-and-connection",
            b"HTTP/1.1 200 OK\r\nKeep-Alive: timeout=5\r\nConnection: keep-alive\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .without(&["timeout=5"]),
        case(
            "proxy-authenticate-and-te",
            b"HTTP/1.1 200 OK\r\nProxy-Authenticate: Basic\r\nTE: trailers\r\nTrailer: X\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .without(&["basic", "trailers"]),
    ])
    .await;
}

#[tokio::test]
async fn interim_responses_are_forwarded_and_the_final_one_is_awaited_like_on_the_tokio_plane() {
    agree(&[
        case(
            "100-continue-then-200",
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 100 Continue\r\nx-request-id: X\r\n\r\nHTTP/1.1 200 OK\r\n",
        )
        .with(&["\r\n\r\nok<open>"]),
        case(
            "103-early-hints-then-200",
            b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>; rel=preload\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 103 Early Hints\r\nlink: </a.css>; rel=preload\r\nx-request-id: X\r\n\r\nHTTP/1.1 200 OK\r\n",
        ),
        case(
            "100-and-103-then-200",
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 100 Continue\r\n",
        )
        .with(&["103 early hints", "200 ok"]),
        case(
            "unknown-1xx-then-200",
            b"HTTP/1.1 199 Odd\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 199 Odd\r\n",
        ),
        case(
            "interim-with-connection-and-upgrade-fields",
            b"HTTP/1.1 103 Early Hints\r\nConnection: x-a\r\nX-A: 1\r\nUpgrade: h2c\r\nLink: </b>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 103 Early Hints\r\n",
        )
        .without(&["x-a", "h2c"]),
        case(
            "bare-lf-interim-then-200",
            b"HTTP/1.1 103 Early Hints\nLink: </a>\n\nHTTP/1.1 200 OK\nContent-Length: 2\n\nok",
            "HTTP/1.1 103 Early Hints\r\n",
        ),
        case(
            "interim-and-head-of-the-final-response-in-a-chunked-answer",
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            "HTTP/1.1 100 Continue\r\n",
        )
        .with(&["transfer-encoding: chunked", "2\r\nok\r\n0\r\n\r\n"]),
        case(
            "interim-and-then-the-origin-closes",
            b"HTTP/1.1 100 Continue\r\n\r\n",
            "HTTP/1.1 100 Continue\r\n",
        )
        .closing()
        .with(&["502 bad gateway"]),
        case(
            "switching-protocols-nobody-asked-for",
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\nabc",
            BAD_GATEWAY,
        ),
        case(
            "switching-protocols-after-an-interim",
            b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 101 Switching Protocols\r\n\r\n",
            "HTTP/1.1 103 Early Hints\r\n",
        )
        .with(&["502 bad gateway"]),
    ])
    .await;
}

#[tokio::test]
async fn framing_the_tokio_plane_accepts_is_relayed_like_on_the_tokio_plane() {
    agree(&[
        case(
            "transfer-encoding-without-chunked",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nxx",
            "HTTP/1.1 200 OK\r\n",
        )
        .closing()
        .with(&["transfer-encoding: chunked\r\n", "2\r\nxx\r\n0\r\n\r\n<open>"]),
        case(
            "transfer-encoding-chunked-then-gzip",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\n",
        )
        .closing()
        .with(&["transfer-encoding: chunked\r\n"]),
        case(
            "transfer-encoding-identity",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\n\r\nxx",
            "HTTP/1.1 200 OK\r\n",
        )
        .closing(),
        case(
            "transfer-encoding-folded-after-chunked",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n ,gzip\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\n",
        )
        .closing(),
        case(
            "gzip-then-chunked",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["2\r\nok\r\n0\r\n\r\n<open>"]),
        case(
            "length-next-to-chunked",
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\n",
        )
        .without(&["content-length"]),
        case(
            "invalid-length-next-to-chunked",
            b"HTTP/1.1 200 OK\r\nContent-Length: abc\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\n",
        )
        .without(&["content-length"]),
        case(
            "length-twice-with-the-same-value",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["\r\n\r\nok<open>"]),
        case(
            "length-list-with-the-same-value",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2, 2\r\n\r\nok",
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["\r\n\r\nok<open>"]),
        case(
            "http-1-0-until-close",
            b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nok-until-close",
            "HTTP/1.1 200 OK\r\n",
        )
        .closing()
        .with(&["e\r\nok-until-close\r\n0\r\n\r\n"]),
        case(
            "no-body-with-205-and-a-length",
            b"HTTP/1.1 205 Reset\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 205 Reset\r\n",
        ),
        case(
            "204-with-a-length",
            b"HTTP/1.1 204 No Content\r\nContent-Length: 2\r\n\r\n",
            "HTTP/1.1 204 No Content\r\n",
        ),
    ])
    .await;
}

#[tokio::test]
async fn lengths_and_header_bytes_the_tokio_plane_refuses_are_a_bad_gateway_on_both_planes() {
    agree(&[
        case(
            "lengths-that-differ",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nokx",
            BAD_GATEWAY,
        ),
        case(
            "length-list-that-differs",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2, 3\r\n\r\nokx",
            BAD_GATEWAY,
        ),
        case(
            "length-with-a-sign",
            b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "length-with-a-space-inside",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "length-folded",
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "length-too-large",
            b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "control-byte-in-a-value",
            b"HTTP/1.1 200 OK\r\nX-A: a\x01b\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "nul-in-a-value",
            b"HTTP/1.1 200 OK\r\nX-A: a\x00b\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "space-inside-a-name",
            b"HTTP/1.1 200 OK\r\nX A: a\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "line-without-a-colon",
            b"HTTP/1.1 200 OK\r\nbroken\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "empty-name",
            b"HTTP/1.1 200 OK\r\n: a\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
    ])
    .await;
}

#[tokio::test]
async fn connection_options_and_start_lines_the_tokio_plane_refuses_are_a_bad_gateway_on_both_planes()
 {
    agree(&[
        case(
            "connection-names-the-length",
            b"HTTP/1.1 200 OK\r\nConnection: Content-Length\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "connection-names-the-transfer-encoding",
            b"HTTP/1.1 200 OK\r\nConnection: Transfer-Encoding\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "connection-with-an-option-of-two-words",
            b"HTTP/1.1 200 OK\r\nConnection: two words\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "status-below-100",
            b"HTTP/1.1 099 X\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "status-of-two-digits",
            b"HTTP/1.1 20 X\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "lowercase-version",
            b"http/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "version-1-2",
            b"HTTP/1.2 200 OK\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "two-spaces-after-the-version",
            b"HTTP/1.1  200 OK\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "bare-cr-in-a-value",
            b"HTTP/1.1 200 OK\r\nX-A: a\rb\r\nContent-Length: 2\r\n\r\nok",
            BAD_GATEWAY,
        ),
        case(
            "chunk-size-that-is-not-hex",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nok\r\n0\r\n\r\n",
            "<EOF>",
        ),
    ])
    .await;
}

#[tokio::test]
async fn statuses_and_heads_of_any_size_up_to_the_limit_are_relayed_like_on_the_tokio_plane() {
    let wide = {
        let mut head = b"HTTP/1.1 200 OK\r\n".to_vec();
        for index in 0..255 {
            head.extend_from_slice(format!("X-{index}: v\r\n").as_bytes());
        }
        head.extend_from_slice(b"Content-Length: 2\r\n\r\nok");
        leaked(head)
    };
    let too_wide = {
        let mut head = b"HTTP/1.1 200 OK\r\n".to_vec();
        for index in 0..256 {
            head.extend_from_slice(format!("X-{index}: v\r\n").as_bytes());
        }
        head.extend_from_slice(b"Content-Length: 2\r\n\r\nok");
        leaked(head)
    };
    agree(&[
        case(
            "status-600",
            b"HTTP/1.1 600 X\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 600 X\r\n",
        ),
        case(
            "status-999",
            b"HTTP/1.1 999 X\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 999 X\r\n",
        ),
        case(
            "head-of-20-kib-in-100-headers",
            head_of(100, 200),
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-h99: "]),
        case(
            "head-of-100-kib-in-5-headers",
            head_of(5, 20_000),
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-h4: "]),
        case(
            "head-of-16-kib-in-1-header",
            head_of(1, 16_300),
            "HTTP/1.1 200 OK\r\n",
        ),
        case(
            "head-of-17-kib-in-1-header",
            head_of(1, 17_000),
            "HTTP/1.1 200 OK\r\n",
        ),
        case(
            "head-of-900-kb-in-60-headers",
            head_of(60, 15_000),
            "HTTP/1.1 200 OK\r\n",
        )
        .with(&["x-h59: "]),
        case(
            "head-of-2-mb-in-30-headers",
            head_of(30, 70_000),
            BAD_GATEWAY,
        ),
        case("255-headers-and-a-length", wide, "HTTP/1.1 200 OK\r\n").with(&["x-254: v\r\n"]),
        case("256-headers-and-a-length", too_wide, BAD_GATEWAY),
    ])
    .await;
}

async fn paced_origin(pieces: Vec<(&'static [u8], Duration)>) -> Origin {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().expect("origin address");
    let pieces = Arc::new(pieces);
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            let pieces = Arc::clone(&pieces);
            tokio::spawn(async move {
                stream.set_nodelay(true).expect("origin nodelay");
                while h1_wire::read_request(&mut stream).await.is_some() {
                    for (piece, pause) in pieces.iter() {
                        if stream.write_all(piece).await.is_err() {
                            return;
                        }
                        sleep(*pause).await;
                    }
                }
            });
        }
    });
    Origin {
        address,
        requests: Arc::default(),
    }
}

async fn paced_answer(plane: HttpDataPlane, pieces: Vec<(&'static [u8], Duration)>) -> Vec<u8> {
    let wire = Wire::over(paced_origin(pieces).await, logged(plane)).await;
    let answer = answer(wire.proxy).await;
    wire.finish();
    answer
}

const PAUSE: Duration = Duration::from_millis(40);

fn pieces_of(bytes: &'static [u8], size: usize) -> Vec<(&'static [u8], Duration)> {
    bytes.chunks(size).map(|piece| (piece, PAUSE)).collect()
}

#[tokio::test]
async fn a_head_that_arrives_in_pieces_is_read_whole_like_on_the_tokio_plane() {
    let clean = head_of(40, 700);
    let bare_lf = leaked(
        head_of(40, 700)
            .iter()
            .copied()
            .filter(|byte| *byte != b'\r')
            .collect(),
    );
    let folded = {
        let mut head = b"HTTP/1.1 200 OK\r\n".to_vec();
        for index in 0..30 {
            head.extend_from_slice(format!("X-{index}: a\r\n {}\r\n", "b".repeat(900)).as_bytes());
        }
        head.extend_from_slice(b"Content-Length: 2\r\n\r\nok");
        leaked(head)
    };
    for (name, reply, size) in [
        ("clean in pieces of 3000", clean, 3_000),
        ("clean in pieces of 16 KiB", clean, 16_384),
        ("clean in pieces of 16382", clean, 16_382),
        ("bare LF in pieces of 5000", bare_lf, 5_000),
        ("folded in pieces of 7000", folded, 7_000),
        ("clean in tiny pieces", &clean[..3_000], 1_000),
    ] {
        let reply = if name == "clean in tiny pieces" {
            head_of(3, 700)
        } else {
            reply
        };
        let theirs = paced_answer(HttpDataPlane::Tokio, pieces_of(reply, size)).await;
        let ours = paced_answer(HttpDataPlane::Epoll, pieces_of(reply, size)).await;
        assert!(
            ours == theirs,
            "{name}: tokio {:?}, epoll {:?}",
            clipped(&text(&theirs)),
            clipped(&text(&ours))
        );
        assert!(text(&ours).starts_with("HTTP/1.1 200 OK\r\n"), "{name}");
        assert!(text(&ours).contains("ok<OPEN>"), "{name}");
    }
}

const EARLY_HINTS: &[u8] = b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>; rel=preload\r\n\r\n";
const FINAL: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

#[tokio::test]
async fn an_interim_response_reaches_the_client_before_the_final_one_exists() {
    for plane in [HttpDataPlane::Tokio, HttpDataPlane::Epoll] {
        let pause = Duration::from_millis(600);
        let origin = paced_origin(vec![(EARLY_HINTS, pause), (FINAL, Duration::ZERO)]).await;
        let wire = Wire::over(origin, logged(plane)).await;
        let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
        client.write_all(GET).await.expect("write request");
        let mut first = vec![0_u8; 4096];
        let count = timeout(pause / 2, client.read(&mut first))
            .await
            .unwrap_or_else(|_| panic!("{plane:?}: no interim response within {:?}", pause / 2))
            .expect("read");
        let first = normalized(&first[..count]);
        assert!(
            first.starts_with(b"HTTP/1.1 103 Early Hints\r\nlink: </a.css>; rel=preload\r\n"),
            "{plane:?}: {:?}",
            text(&first)
        );
        assert!(!text(&first).contains("200 OK"), "{plane:?}");
        let rest = answer_of(&mut client).await;
        assert!(
            text(&rest).starts_with("HTTP/1.1 200 OK\r\n") && text(&rest).contains("ok<OPEN>"),
            "{plane:?}: {:?}",
            text(&rest)
        );
        wire.finish();
    }
}

#[tokio::test]
async fn the_connection_serves_the_next_request_after_an_interim_and_a_final_response() {
    for plane in [HttpDataPlane::Tokio, HttpDataPlane::Epoll] {
        let both: &'static [u8] = leaked([EARLY_HINTS, FINAL].concat());
        let origin = Origin::start(both).await;
        let wire = Wire::over(origin, logged(plane)).await;
        let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
        for round in 0..3 {
            client.write_all(GET).await.expect("write request");
            let mut seen = Vec::new();
            let mut buffer = [0_u8; 4096];
            while !seen.ends_with(b"ok") {
                let count = timeout(STEP, client.read(&mut buffer))
                    .await
                    .expect("response")
                    .expect("read");
                assert_ne!(
                    count,
                    0,
                    "{plane:?} round {round}: closed after {:?}",
                    text(&seen)
                );
                seen.extend_from_slice(&buffer[..count]);
            }
            let shown = text(&normalized(&seen));
            assert!(
                shown.starts_with("HTTP/1.1 103 Early Hints\r\n")
                    && shown.contains("\r\n\r\nHTTP/1.1 200 OK\r\n"),
                "{plane:?} round {round}: {shown:?}"
            );
        }
        wire.finish();
    }
}
