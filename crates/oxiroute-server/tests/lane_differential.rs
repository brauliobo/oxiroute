#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The fast lane against the classic path: the same bytes go to two real `oxiroute` processes, one with the lane
//! (the default) and one with `http_fast_lane` off, in front of identical origins. Everything a client or an
//! origin can observe must be identical: the bytes the origin receives, the bytes the client gets back, and
//! whether the connection stays open. Dates and generated request ids are replaced by markers.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

#[path = "lane_differential/corpus.rs"]
mod corpus;

use std::fmt::Write as _;
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path, time::Duration};

use oxiroute_config::{
    ConfigDraft, HttpHostSelector, HttpLiteralHeader, HttpPathSelector, HttpRoute, HttpRouteAction,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::{sleep, timeout},
};

use h1_wire::{Origin, STEP, Wire};

const QUIET: Duration = Duration::from_millis(300);

/// A fixed route in front of the catch-all proxy route, so both lanes get requests.
fn with_fixed_route(config: &mut ConfigDraft) {
    let route = HttpRoute {
        host: None,
        path: HttpPathSelector::SegmentPrefix {
            value: "/fixed".into(),
        },
        methods: Vec::new(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::FixedResponse {
            status: 200,
            body: "fixed\n".into(),
            headers: vec![HttpLiteralHeader {
                name: "x-fixed".into(),
                value: "yes".into(),
                always: true,
            }],
        },
    };
    config.http_services[0].routes.insert(0, route);
}

/// A static files route in front of the others, as a service that mixes file serving with lane routes has.
fn with_static_route<'a>(root: &'a Path, host: Option<&'a str>) -> impl Fn(&mut ConfigDraft) + 'a {
    move |config| {
        let route = HttpRoute {
            host: host.map(|value| HttpHostSelector::ExactAuthority {
                value: value.into(),
            }),
            path: HttpPathSelector::SegmentPrefix {
                value: "/static".into(),
            },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: serde_json::from_value(
                serde_json::json!({ "type": "static_files", "root_directory": root }),
            )
            .expect("static files action"),
        };
        config.http_services[0].routes.insert(0, route);
    }
}

struct Pair {
    classic: Wire,
    lane: Wire,
}

impl Pair {
    async fn start(reply: &'static [u8]) -> Self {
        Self::serving(reply, false).await
    }

    /// Both proxies serve the files of `root` under `/static`, next to the fixed and the proxied route.
    async fn mixed(root: &Path, host: Option<&str>) -> Self {
        let classic = Wire::over(Origin::start(OK).await, |config| {
            with_fixed_route(config);
            with_static_route(root, host)(config);
            config.http_fast_lane = Some(false);
        })
        .await;
        let lane = Wire::over(Origin::start(OK).await, |config| {
            with_fixed_route(config);
            with_static_route(root, host)(config);
        })
        .await;
        Self { classic, lane }
    }

    async fn serving(reply: &'static [u8], closes: bool) -> Self {
        let origin = |closes| async move {
            if closes {
                Origin::start_closing(reply).await
            } else {
                Origin::start(reply).await
            }
        };
        let classic = Wire::over(origin(closes).await, |config| {
            with_fixed_route(config);
            config.http_fast_lane = Some(false);
        })
        .await;
        let lane = Wire::over(origin(closes).await, with_fixed_route).await;
        Self { classic, lane }
    }

    fn finish(self) {
        self.classic.finish();
        self.lane.finish();
    }
}

/// What a client and the origin saw for a script of writes on one connection.
#[derive(Debug, Eq, PartialEq)]
struct Outcome {
    response: String,
    origin: Vec<String>,
    closed: bool,
}

async fn play(wire: &Wire, script: &[&[u8]]) -> Outcome {
    let before = wire.origin.requests().len();
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    let mut bytes = Vec::new();
    let mut closed = false;
    let mut buffer = [0_u8; 8192];
    for write in script {
        let _ = stream.write_all(write).await;
        loop {
            match timeout(QUIET, stream.read(&mut buffer)).await {
                Ok(Ok(0) | Err(_)) => {
                    closed = true;
                    break;
                }
                Ok(Ok(count)) => bytes.extend_from_slice(&buffer[..count]),
                Err(_) => break,
            }
        }
        if closed {
            break;
        }
    }
    let origin = wire.origin.requests().split_off(before);
    Outcome {
        response: merge_chunks(&normalize(&String::from_utf8_lossy(&bytes))),
        origin: origin.iter().map(|request| normalize(request)).collect(),
        closed,
    }
}

/// Dates and generated request ids differ per request by construction.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive("\r\n") {
        if let Some(rest) = line.strip_prefix("Date: ") {
            out.push_str("Date: <date>");
            out.push_str(&rest[rest.find("\r\n").unwrap_or(rest.len())..]);
        } else {
            out.push_str(&mask_ids(line));
        }
    }
    out
}

/// The classic path writes the body pieces that are ready together as one chunk, the lane writes one chunk per piece:
/// the sizes of chunks carry no meaning, so a chunked body is compared as one chunk.
fn merge_chunks(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(end) = rest.find("\r\n\r\n").map(|at| at + 4) {
        let (head, tail) = rest.split_at(end);
        out.push_str(head);
        rest = tail;
        if head.contains("\r\nTransfer-Encoding: chunked\r\n")
            && let Some((body, after)) = take_chunks(rest)
        {
            let _ = write!(out, "{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The joined chunks at the start of `text` and what follows the last chunk, if `text` starts with a whole body.
fn take_chunks(text: &str) -> Option<(String, &str)> {
    let mut body = String::new();
    let mut rest = text;
    loop {
        let (size, after) = rest.split_once("\r\n")?;
        let size = usize::from_str_radix(size, 16).ok()?;
        if size == 0 {
            return Some((body, after.strip_prefix("\r\n")?));
        }
        body.push_str(after.get(..size)?);
        rest = after.get(size..)?.strip_prefix("\r\n")?;
    }
}

fn mask_ids(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let id = bytes.get(index..index + 24).filter(|window| {
            window.starts_with(b"rq-")
                && window[11] == b'-'
                && window[3..11]
                    .iter()
                    .chain(&window[12..])
                    .all(u8::is_ascii_hexdigit)
        });
        if id.is_some() {
            out.push_str("rq-<id>");
            index += 24;
        } else {
            out.push(char::from(bytes[index]));
            index += 1;
        }
    }
    out
}

/// Runs every case of `cases` against both proxies and reports every difference at once.
async fn compare(pair: &Pair, cases: &[corpus::Case]) {
    let mut differences = Vec::new();
    for case in cases {
        let script = case.script();
        let classic = play(&pair.classic, &script).await;
        let lane = play(&pair.lane, &script).await;
        if classic != lane {
            differences.push(format!(
                "case `{}`\n classic: {classic:#?}\n lane: {lane:#?}",
                case.name
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} of {} cases differ:\n{}",
        differences.len(),
        cases.len(),
        differences.join("\n")
    );
}

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Origin: yes\r\n\r\nok";

#[tokio::test]
async fn proxied_requests_are_identical_on_both_paths() {
    let pair = Pair::start(OK).await;
    compare(&pair, &corpus::requests("/r")).await;
    pair.finish();
}

#[tokio::test]
async fn fixed_responses_are_identical_on_both_paths() {
    let pair = Pair::start(OK).await;
    compare(&pair, &corpus::requests("/fixed")).await;
    pair.finish();
}

#[tokio::test]
async fn connections_that_alternate_between_the_lane_and_the_classic_path_are_identical() {
    let pair = Pair::start(OK).await;
    compare(&pair, &corpus::sequences()).await;
    pair.finish();
}

#[tokio::test]
async fn proxied_responses_of_every_shape_are_identical_on_both_paths() {
    let mut differences = Vec::new();
    for (name, reply, closes) in corpus::replies() {
        if name == SWITCHING_PROTOCOLS {
            continue;
        }
        let pair = Pair::serving(reply, closes).await;
        for case in corpus::probes("/r") {
            // A request that arrives while a response still streams closes the connection on the classic path, which
            // watches the client; the lane serves it after the response, which never ends here.
            if name == STREAMING && case.name == "two on one connection" {
                continue;
            }
            let script = case.script();
            let classic = play(&pair.classic, &script).await;
            let lane = play(&pair.lane, &script).await;
            if classic != lane {
                differences.push(format!(
                    "reply `{name}`, request `{}`\n classic: {classic:#?}\n lane: {lane:#?}",
                    case.name
                ));
            }
        }
        pair.finish();
    }
    assert!(
        differences.is_empty(),
        "{} differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

const SWITCHING_PROTOCOLS: &str = "switching protocols";
const STREAMING: &str = "close delimited, origin stays open";

/// The one deliberate difference: an origin that answers `101` to a request that asked for no upgrade is relayed by the
/// classic path and is a bad gateway on the lane, which serves no upgraded connections.
#[tokio::test]
async fn an_unrequested_switching_protocols_reply_is_a_bad_gateway_on_the_lane() {
    let (_, reply, closes) = corpus::replies()
        .into_iter()
        .find(|(name, ..)| *name == SWITCHING_PROTOCOLS)
        .expect("the corpus has a switching protocols reply");
    let pair = Pair::serving(reply, closes).await;
    let probe = corpus::probes("/r").remove(0);
    let script = probe.script();
    let classic = play(&pair.classic, &script).await;
    let lane = play(&pair.lane, &script).await;
    pair.finish();
    assert!(classic.response.starts_with("HTTP/1.1 101 "), "{classic:?}");
    assert!(lane.response.starts_with("HTTP/1.1 502 "), "{lane:?}");
    assert!(lane.closed);
}

const STATIC_FILES: [(&str, usize); 3] = [
    ("small.txt", 5),
    ("one-k.bin", 1024),
    ("large.bin", 1024 * 1024 + 17),
];

fn static_root() -> tempfile::TempDir {
    let directory = tempfile::tempdir_in("/dev/shm").expect("static root directory in memory");
    let root = directory.path().join("public");
    fs::create_dir(&root).expect("create static root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure static root");
    for (name, length) in STATIC_FILES {
        let body: Vec<u8> = (0..length)
            .map(|at| b'a' + u8::try_from(at % 26).expect("below 26"))
            .collect();
        fs::write(root.join(name), body).expect("write a static file");
    }
    directory
}

fn static_cases() -> Vec<corpus::Case> {
    let mut cases = corpus::requests("/static/small.txt");
    cases.extend(corpus::static_requests());
    cases
}

/// A request for a route the lane does not serve is handed over unread: the bytes of every static file response, its
/// conditional and range forms, and the connection that follows must be the classic path's.
#[tokio::test]
async fn static_file_requests_are_identical_on_both_paths() {
    let directory = static_root();
    let pair = Pair::mixed(&directory.path().join("public"), None).await;
    compare(&pair, &static_cases()).await;
    pair.finish();
}

#[tokio::test]
async fn connections_that_alternate_between_static_files_and_the_lane_are_identical() {
    let directory = static_root();
    let pair = Pair::mixed(&directory.path().join("public"), None).await;
    compare(&pair, &corpus::static_sequences()).await;
    pair.finish();
}

/// A route bound to a host makes the lane read the `Host` field before it decides, and a request for another host
/// reaches the proxied route.
#[tokio::test]
async fn static_file_requests_on_a_host_bound_route_are_identical_on_both_paths() {
    let directory = static_root();
    let pair = Pair::mixed(&directory.path().join("public"), Some("example.test")).await;
    compare(&pair, &corpus::static_requests()).await;
    compare(&pair, &corpus::static_sequences()).await;
    let elsewhere = corpus::static_requests_for("other.test");
    compare(&pair, &elsewhere).await;
    pair.finish();
}
