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
#[path = "lane_differential/features.rs"]
mod features;
#[path = "lane_differential/reading.rs"]
mod reading;
#[path = "lane_differential/retries.rs"]
mod retries;

use std::fmt::Write as _;
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path, time::Duration};

use oxiroute_config::{
    ConfigDraft, HttpHostSelector, HttpLiteralHeader, HttpPathSelector, HttpRedirectLocation,
    HttpRoute, HttpRouteAction,
};
use tokio::{io::AsyncWriteExt as _, net::TcpStream, time::sleep};

use h1_wire::{Origin, STEP, Wire};

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
    let moved = HttpRoute {
        path: HttpPathSelector::SegmentPrefix {
            value: "/moved".into(),
        },
        action: HttpRouteAction::Redirect {
            status: 308,
            location: HttpRedirectLocation::Literal {
                value: "https://example.com/new".into(),
            },
            headers: vec![HttpLiteralHeader {
                name: "x-moved".into(),
                value: "yes".into(),
                always: true,
            }],
        },
        ..config.http_services[0].routes[0].clone()
    };
    config.http_services[0].routes.insert(1, moved);
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
            action: serde_json::from_value(serde_json::json!({
                "type": "static_files",
                "root_directory": root,
                "path_mapping": "alias"
            }))
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

    /// [`Self::mixed`] with the static cache holding a file for `window_ms`, and every file requested once so that
    /// the cache holds it.
    async fn mixed_holding(root: &Path, host: Option<&str>, window_ms: u64) -> Self {
        let window = window_ms.to_string();
        let environment = [(STATIC_VALIDITY, window.as_str())];
        let classic = Wire::over_with_env(Origin::start(OK).await, &environment, |config| {
            with_fixed_route(config);
            with_static_route(root, host)(config);
            config.http_fast_lane = Some(false);
        })
        .await;
        let lane = Wire::over_with_env(Origin::start(OK).await, &environment, |config| {
            with_fixed_route(config);
            with_static_route(root, host)(config);
        })
        .await;
        let pair = Self { classic, lane };
        for (name, _) in STATIC_FILES {
            let request = format!(
                "GET /static/{name} HTTP/1.1\r\nHost: {}\r\n\r\n",
                host.unwrap_or("example.test")
            );
            for wire in [&pair.classic, &pair.lane] {
                wire.exchange(request.as_bytes()).await;
            }
        }
        pair
    }

    async fn serving(reply: &'static [u8], closes: bool) -> Self {
        Self::configured(reply, closes, false, |_| {}).await
    }

    /// Both proxies in front of an origin of their own that replies the same, with `edit` applied to both
    /// configurations; `tls` puts the origins behind TLS.
    async fn configured(
        reply: &'static [u8],
        closes: bool,
        tls: bool,
        edit: impl Fn(&mut ConfigDraft),
    ) -> Self {
        let origin = || async move {
            match (tls, closes) {
                (true, closes) => Origin::start_tls(reply, closes).await,
                (false, true) => Origin::start_closing(reply).await,
                (false, false) => Origin::start(reply).await,
            }
        };
        let classic = Wire::over(origin().await, |config| {
            with_fixed_route(config);
            edit(config);
            config.http_fast_lane = Some(false);
        })
        .await;
        let lane = Wire::over(origin().await, |config| {
            with_fixed_route(config);
            edit(config);
        })
        .await;
        Self { classic, lane }.warmed().await
    }

    /// The first request of a process pays for what it sets up (connections, tables): the comparison starts after
    /// one request has been served by each.
    async fn warmed(self) -> Self {
        self.play(&[b"GET /r HTTP/1.1\r\nHost: warm.test\r\n\r\n"])
            .await;
        self
    }

    /// `script` on a connection to each proxy at the same time.
    async fn play(&self, script: &[&[u8]]) -> (Outcome, Outcome) {
        tokio::join!(play(&self.classic, script), play(&self.lane, script))
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
    let mut unanswered = Vec::new();
    let mut closed = false;
    for write in script {
        let _ = stream.write_all(write).await;
        unanswered.extend_from_slice(write);
        let answered = bytes.len();
        closed = reading::read_step(&mut stream, &unanswered, &mut bytes).await;
        if bytes.len() > answered {
            unanswered.clear();
        }
        if closed {
            break;
        }
    }
    // The origin notes a request when it has all of it or its connection closed, and the proxy ends the origin
    // connection of a request cut short only after the client's: the case ends when the origin noted every request
    // it started to receive, or a leftover would show up in the next case. Only the requests it got whole are
    // compared: what a proxy forwards of a request that did not end is not promised.
    drop(stream);
    wire.origin.settled().await;
    let origin = wire.origin.requests().split_off(before);
    let mut origin: Vec<_> = origin
        .iter()
        .filter(|request| reading::is_whole_request(request))
        .map(|request| merge_chunks(&normalize(request)))
        .collect();
    // Once the client side closed, the origin connections of the proxy end on their own: the order in which the
    // origin notes the requests cut short by that is not the proxy's to promise.
    if closed {
        origin.sort();
    }
    Outcome {
        response: merge_chunks(&normalize(&String::from_utf8_lossy(&bytes))),
        origin,
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
        let (classic, lane) = pair.play(&script).await;
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
async fn literal_redirects_are_identical_on_both_paths() {
    let pair = Pair::start(OK).await;
    compare(&pair, &corpus::requests("/moved")).await;
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
            let (classic, lane) = pair.play(&script).await;
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
    let (classic, lane) = pair.play(&script).await;
    pair.finish();
    assert!(classic.response.starts_with("HTTP/1.1 101 "), "{classic:?}");
    assert!(lane.response.starts_with("HTTP/1.1 502 "), "{lane:?}");
    assert!(lane.closed);
}

/// The test hook that sets how long the static cache holds a file, in milliseconds.
const STATIC_VALIDITY: &str = "OXIROUTE_INTERNAL_TEST_STATIC_VALIDITY_MS";

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

/// With the cache holding every file, the lane answers the plain requests itself and hands the conditional, range and
/// other ones to the classic path: the bytes and the connection must still be the classic path's.
#[tokio::test]
async fn static_file_requests_answered_from_the_cache_are_identical_on_both_paths() {
    let directory = static_root();
    let pair = Pair::mixed_holding(&directory.path().join("public"), None, 600_000).await;
    compare(&pair, &static_cases()).await;
    compare(&pair, &corpus::static_sequences()).await;
    pair.finish();
}

#[tokio::test]
async fn static_file_requests_on_a_host_bound_route_answered_from_the_cache_are_identical() {
    let directory = static_root();
    let root = directory.path().join("public");
    let pair = Pair::mixed_holding(&root, Some("example.test"), 600_000).await;
    compare(&pair, &corpus::static_requests()).await;
    compare(&pair, &corpus::static_sequences()).await;
    compare(&pair, &corpus::static_requests_for("other.test")).await;
    pair.finish();
}

/// A file replaced while the cache holds it is served as it was, whole, until the window ends; then the next request
/// sees the new file. The classic path and the lane agree at every step.
#[tokio::test]
async fn a_file_replaced_while_cached_is_served_as_it_was_until_the_window_ends() {
    let directory = static_root();
    let root = directory.path().join("public");
    let pair = Pair::mixed_holding(&root, None, 6_000).await;
    let get = |file: &str| format!("GET /static/{file} HTTP/1.1\r\nHost: example.test\r\n\r\n");
    let old = play(&pair.lane, &[get("large.bin").as_bytes()]).await;
    for name in ["large.bin", "small.txt"] {
        let replacement = root.join(format!("{name}.new"));
        let length = if name == "small.txt" {
            7
        } else {
            2 * 1024 * 1024
        };
        fs::write(&replacement, vec![b'Z'; length]).expect("write the replacement");
        fs::rename(&replacement, root.join(name)).expect("replace the file");
    }
    let cases = [get("large.bin"), get("small.txt")];
    for case in &cases {
        let classic = play(&pair.classic, &[case.as_bytes()]).await;
        let lane = play(&pair.lane, &[case.as_bytes()]).await;
        assert_eq!(classic, lane, "inside the window: {case}");
    }
    let held = play(&pair.lane, &[cases[0].as_bytes()]).await;
    assert_eq!(held.response, old.response, "the held file is served whole");

    sleep(Duration::from_millis(6_500)).await;
    for case in &cases {
        // The request that finds the entry expired reads the file again, which a loaded host can take longer than the
        // quiet period of `play`: ask until the new file arrives, then compare the two paths.
        for wire in [&pair.classic, &pair.lane] {
            for _ in 0..20 {
                if play(wire, &[case.as_bytes()])
                    .await
                    .response
                    .contains("ZZZZZZZ")
                {
                    break;
                }
            }
        }
        let classic = play(&pair.classic, &[case.as_bytes()]).await;
        let lane = play(&pair.lane, &[case.as_bytes()]).await;
        assert_eq!(classic, lane, "after the window: {case}");
        assert!(
            lane.response.contains("ZZZZZZZ"),
            "the new file: {case} ({} bytes, closed {}) {:?}",
            lane.response.len(),
            lane.closed,
            lane.response.chars().take(400).collect::<String>()
        );
    }
    pair.finish();
}

/// An origin notes a request cut short when its connection ends; `settled` waits for that, so the next case of a
/// script never sees the leftover of the previous one.
#[tokio::test]
async fn an_origin_settles_when_the_request_it_started_to_receive_is_noted() {
    let origin = Origin::start(OK).await;
    let mut stream = TcpStream::connect(origin.address).await.expect("connect");
    stream
        .write_all(
            b"POST /r HTTP/1.1\r\nHost: example.test\r\nContent-Length: 30\r\n\r\n0123456789",
        )
        .await
        .expect("write a part of the request");
    while origin.receiving() == 0 {
        tokio::task::yield_now().await;
    }
    assert!(
        origin.requests().is_empty(),
        "not noted before its connection ends"
    );
    drop(stream);
    origin.settled().await;
    assert_eq!(origin.requests().len(), 1);
    assert_eq!(origin.receiving(), 0);
}
