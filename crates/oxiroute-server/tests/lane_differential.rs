#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The fast lane against the classic path: the same bytes go to two real `oxiroute` processes, one with the lane
//! (the default) and one with `OXIROUTE_HTTP_LANE=off`, in front of identical origins. Everything a client or an
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

use std::time::Duration;

use oxiroute_config::{
    ConfigDraft, HttpLiteralHeader, HttpPathSelector, HttpRoute, HttpRouteAction,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::{sleep, timeout},
};

use h1_wire::{Origin, STEP, Wire};

const QUIET: Duration = Duration::from_millis(150);

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

struct Pair {
    classic: Wire,
    lane: Wire,
}

impl Pair {
    async fn start(reply: &'static [u8]) -> Self {
        Self::serving(reply, false).await
    }

    async fn serving(reply: &'static [u8], closes: bool) -> Self {
        let origin = |closes| async move {
            if closes {
                Origin::start_closing(reply).await
            } else {
                Origin::start(reply).await
            }
        };
        let classic = Wire::over_with_env(
            origin(closes).await,
            with_fixed_route,
            &[("OXIROUTE_HTTP_LANE", "off")],
        )
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
        response: normalize(&String::from_utf8_lossy(&bytes)),
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
        let pair = Pair::serving(reply, closes).await;
        for case in corpus::probes("/r") {
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
