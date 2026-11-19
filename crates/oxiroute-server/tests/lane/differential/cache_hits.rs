//! A route with a memory cache on the lane against the classic path: both proxies fill their cache from an origin of
//! their own and answer the same requests, and what the clients and the origins saw must be identical, hits included.
//! The age of a hit is the one thing that differs by construction (two processes, two clocks), so it is masked.

use oxiroute_config::{AccessLogPolicy, ConfigDraft};

use futures_util::{StreamExt as _, stream};

use super::{Fleet, Outcome, Pair, SHARDS, compare_viewed, corpus, features::log_lines};
use crate::{
    h1_wire::{Origin, Wire},
    revalidating_origin::with_cache_on,
};

const CACHEABLE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: text/plain\r\nETag: \"v1\"\r\n\
Last-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\nCache-Control: max-age=60\r\nX-Origin: yes\r\n\r\nok";

/// The proxy route, the last of the service, with the memory cache.
fn cached(config: &mut ConfigDraft) {
    let route = config.http_services[0].routes.len() - 1;
    with_cache_on(config, route);
}

fn age_masked(mut outcome: Outcome) -> Outcome {
    outcome.response = outcome
        .response
        .split_inclusive("\r\n")
        .map(|line| {
            if line.starts_with("Age: ") {
                "Age: <age>\r\n"
            } else {
                line
            }
        })
        .collect();
    outcome
}

async fn compare(fleet: &Fleet, cases: &[corpus::Case]) {
    compare_viewed(fleet, cases, |_| false, age_masked).await;
}

#[tokio::test]
async fn cached_responses_are_identical_on_both_paths() {
    let fleet = Fleet::of(|_| Pair::configured(CACHEABLE, false, false, cached)).await;
    let cases: Vec<_> = [corpus::cache_requests("/r"), corpus::requests("/r")]
        .into_iter()
        .flatten()
        .collect();
    compare(&fleet, &cases).await;
    fleet.finish();
}

fn large_reply() -> &'static [u8] {
    let body = (0..40_000_u32)
        .map(|at| b'a' + u8::try_from(at % 23).expect("below 23"))
        .collect::<Vec<_>>();
    let head = "HTTP/1.1 200 OK\r\nContent-Length: 40000\r\nCache-Control: max-age=60\r\nETag: \"big\"\r\n\r\n";
    Box::leak([head.as_bytes(), &body].concat().into_boxed_slice())
}

fn shapes() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (
            "empty body",
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nCache-Control: max-age=60\r\nETag: \"e\"\r\n\r\n",
        ),
        (
            "no content",
            b"HTTP/1.1 204 No Content\r\nCache-Control: max-age=60\r\nETag: \"n\"\r\n\r\n",
        ),
        (
            "not found",
            b"HTTP/1.1 404 Not Found\r\nCache-Control: max-age=60\r\nContent-Length: 3\r\n\r\nnop",
        ),
        (
            "vary",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: max-age=60\r\nVary: Accept-Language\r\n\r\nok",
        ),
        (
            "vary on everything",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: max-age=60\r\nVary: *\r\n\r\nok",
        ),
        (
            "repeated and mixed case fields",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nx-a: 1\r\nX-A: 2\r\ncontent-type: text/html\r\n\
Cache-Control: max-age=60\r\nCONTENT-LANGUAGE: en\r\nDate: Mon, 01 Jan 2024 00:00:00 GMT\r\n\r\nok",
        ),
        (
            "an id of the origin",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Request-Id: from-origin\r\nCache-Control: max-age=60\r\n\r\nok",
        ),
        (
            "stale at once",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nETag: \"s\"\r\nCache-Control: max-age=0\r\n\r\nok",
        ),
        (
            "stale while revalidating",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: max-age=0, stale-while-revalidate=60\r\n\r\nok",
        ),
        (
            "must revalidate",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: max-age=60, no-cache\r\nETag: \"m\"\r\n\r\nok",
        ),
        (
            "private",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: private, max-age=60\r\n\r\nok",
        ),
        ("a body above what the lane writes with the head", large_reply()),
    ]
}

#[tokio::test]
async fn cached_responses_of_every_stored_shape_are_identical_on_both_paths() {
    let cases: Vec<_> = [corpus::probes("/r"), corpus::cache_requests("/r")]
        .into_iter()
        .flatten()
        .take(24)
        .collect();
    let cases = &cases;
    stream::iter(shapes().into_iter().map(|(name, reply)| async move {
        let fleet = Fleet(vec![Pair::configured(reply, false, false, cached).await]);
        eprintln!("shape `{name}`");
        compare(&fleet, cases).await;
        fleet.finish();
    }))
    .buffer_unordered(SHARDS)
    .collect::<Vec<_>>()
    .await;
}

#[tokio::test]
async fn access_lines_of_cached_hits_are_identical_on_both_paths() {
    let directory = tempfile::tempdir().expect("log directory");
    let [classic_log, lane_log] =
        ["classic.log", "lane.log"].map(|name| directory.path().join(name));
    let (classic_path, lane_path) = (classic_log.clone(), lane_log.clone());
    let classic = Wire::over(Origin::start(CACHEABLE).await, move |config| {
        super::with_fixed_route(config);
        cached(config);
        config.http_services[0].access_log = Some(AccessLogPolicy::File { path: classic_path });
        config.http_fast_lane = Some(false);
    })
    .await;
    let lane = Wire::over(Origin::start(CACHEABLE).await, move |config| {
        super::with_fixed_route(config);
        cached(config);
        config.http_services[0].access_log = Some(AccessLogPolicy::File { path: lane_path });
    })
    .await;
    let fleet = Fleet(vec![Pair { classic, lane }.warmed().await]);
    compare(&fleet, &corpus::cache_requests("/r")).await;
    fleet.finish_gracefully();
    let (classic_lines, lane_lines) = (log_lines(&classic_log), log_lines(&lane_log));
    assert_ne!(classic_lines.len(), 0);
    assert_eq!(classic_lines, lane_lines);
}
