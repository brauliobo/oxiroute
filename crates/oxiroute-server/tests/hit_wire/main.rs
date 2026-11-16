#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Cache hits of a route on the HTTP/1 fast lane, as seen on the wire: the proxy is the real binary with a memory
//! cache on its route and the origin a real server that records what reaches it. Every test says how many of the
//! requests the lane served (the debug line of its log target) next to what the origin saw: a hit is served by the
//! lane and never reaches the origin, and every request the lane leaves to the classic path (a condition, a range, a
//! stale entry, a miss) is answered there from the same cache.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/h1_wire.rs"]
mod h1_wire;
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/revalidating_origin.rs"]
mod revalidating_origin;

mod client;

use oxiroute_config::{CachePurgeAuthorization, ConfigDraft, HttpRouteAction};

use client::Client;
use h1_wire::{Origin, Step, Wire};
use revalidating_origin::with_cache;

const SERVED: &str = "served by the HTTP/1 lane";
const LOG_TARGET: &str = "oxiroute::h1lane";

fn reply(fields: &str, body: &[u8]) -> &'static [u8] {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{fields}\r\n",
        body.len()
    );
    Box::leak([head.as_bytes(), body].concat().into_boxed_slice())
}

fn body(length: usize) -> Vec<u8> {
    (0..length)
        .map(|at| b'a' + u8::try_from(at % 23).expect("below 23"))
        .collect()
}

const FRESH: &str = "Cache-Control: max-age=60\r\nETag: \"v1\"\r\nContent-Type: text/plain\r\n\
Last-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\n";

async fn wire(origin: Origin, edit: impl FnOnce(&mut ConfigDraft)) -> Wire {
    Wire::over(origin, |config| {
        with_cache(config);
        config.log_filter = Some(format!("{LOG_TARGET}=debug"));
        edit(config);
    })
    .await
}

/// What the origin received and how many requests the lane served.
struct Seen {
    origin: Vec<String>,
    lane: usize,
}

fn stop(wire: Wire) -> Seen {
    let Wire { origin, server, .. } = wire;
    let requests = origin.requests();
    let output = server.shutdown_output();
    Seen {
        origin: requests,
        lane: output.lines().filter(|line| line.contains(SERVED)).count(),
    }
}

#[tokio::test]
async fn fresh_hits_are_served_by_the_lane_and_never_reach_the_origin() {
    for length in [1024, 65536] {
        let payload = body(length);
        let wire = wire(Origin::start(reply(FRESH, &payload)).await, |_| {}).await;
        let mut client = Client::connect(wire.proxy).await;
        let filled = client.get("/item", "").await;
        assert_eq!(filled.status(), "HTTP/1.1 200 OK");
        assert_eq!(filled.field("age"), None);
        for round in 0..10 {
            let hit = client.get("/item", "").await;
            assert_eq!(hit.status(), "HTTP/1.1 200 OK", "round {round}");
            assert_eq!(hit.body, payload, "round {round}");
            assert_eq!(
                hit.field("content-length"),
                Some(length.to_string().as_str())
            );
            assert_eq!(hit.field("etag"), Some("\"v1\""));
            assert_eq!(hit.field("content-type"), Some("text/plain"));
            assert_eq!(hit.field("age"), Some("0"));
            assert_eq!(hit.field("connection"), Some("keep-alive"));
            assert!(
                hit.field("x-request-id")
                    .is_some_and(|id| id.starts_with("rq-"))
            );
            assert!(hit.field("date").is_some());
        }
        let seen = stop(wire);
        assert_eq!(seen.origin.len(), 1, "one request filled the cache");
        assert_eq!(seen.lane, 10, "every hit but the miss is the lane's");
    }
}

#[tokio::test]
async fn the_request_id_of_a_client_is_the_one_a_hit_carries() {
    let wire = wire(Origin::start(reply(FRESH, b"ok")).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    let hit = client.get("/item", "X-Request-Id: client-7\r\n").await;
    assert_eq!(hit.field("x-request-id"), Some("client-7"));
    assert_eq!(stop(wire).lane, 1);
}

#[tokio::test]
async fn head_requests_are_hits_without_a_body() {
    let payload = body(1024);
    let wire = wire(Origin::start(reply(FRESH, &payload)).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    let head = client.request("HEAD", "/item", "").await;
    assert_eq!(head.status(), "HTTP/1.1 200 OK");
    assert_eq!(head.field("content-length"), Some("1024"));
    assert_eq!(head.body, b"");
    let hit = client.get("/item", "").await;
    assert_eq!(
        hit.body, payload,
        "the connection serves the request after a HEAD"
    );
    let seen = stop(wire);
    assert_eq!((seen.origin.len(), seen.lane), (1, 2));
}

#[tokio::test]
async fn a_request_with_a_condition_is_the_origins_to_answer() {
    let payload = body(1024);
    let wire = wire(Origin::start(reply(FRESH, &payload)).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    let conditions = [
        "If-None-Match: \"v1\"\r\n",
        "If-None-Match: W/\"v1\", \"x\"\r\n",
        "If-None-Match: *\r\n",
        "If-Modified-Since: Tue, 01 Jan 2030 00:00:00 GMT\r\n",
        "If-Match: \"v1\"\r\n",
        "If-Unmodified-Since: Tue, 01 Jan 2030 00:00:00 GMT\r\n",
    ];
    for fields in conditions {
        let reply = client.get("/item", fields).await;
        assert_eq!(reply.status(), "HTTP/1.1 200 OK", "{fields}");
        assert_eq!(reply.body, payload, "{fields}");
    }
    let seen = stop(wire);
    assert_eq!(seen.origin.len(), 1 + conditions.len());
    assert_eq!(seen.lane, 0);
}

#[tokio::test]
async fn a_range_goes_to_the_origin_and_leaves_the_entry_as_it_was() {
    let payload = body(1024);
    let wire = wire(Origin::start(reply(FRESH, &payload)).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    client.get("/item", "Range: bytes=0-9\r\n").await;
    client.get("/item", "").await;
    let seen = stop(wire);
    assert_eq!(seen.origin.len(), 2);
    assert!(
        seen.origin[1]
            .to_ascii_lowercase()
            .contains("range: bytes=0-9")
    );
    assert_eq!(seen.lane, 1);
}

#[tokio::test]
async fn request_cache_directives_are_the_caches_to_decide() {
    let payload = body(1024);
    let wire = wire(Origin::start(reply(FRESH, &payload)).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    client.get("/item", "Cache-Control: no-cache\r\n").await;
    client.get("/item", "Cache-Control: max-age=0\r\n").await;
    client.get("/item", "Pragma: no-cache\r\n").await;
    client.get("/item", "Cache-Control: no-store\r\n").await;
    let seen = stop(wire);
    assert_eq!(seen.lane, 0);
    assert!(
        seen.origin.len() >= 2,
        "a request that refuses the entry reaches the origin"
    );
}

#[tokio::test]
async fn a_request_the_entry_may_not_answer_is_not_a_hit() {
    let wire = wire(Origin::start(reply(FRESH, b"ok")).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    let authorized = client
        .get("/item", "Authorization: Bearer secret\r\n")
        .await;
    assert_eq!(authorized.status(), "HTTP/1.1 200 OK");
    let seen = stop(wire);
    assert_eq!(seen.lane, 0);
}

#[tokio::test]
async fn each_host_path_and_query_has_an_entry_of_its_own() {
    let wire = wire(Origin::start(reply(FRESH, b"ok")).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    let other_host = b"GET /item HTTP/1.1\r\nHost: other.test\r\n\r\n";
    for _ in 0..2 {
        for path in ["/item", "/item?a=1", "/item?a=2", "/other", "/item?"] {
            client.get(path, "").await;
        }
        client.send(other_host, false).await;
    }
    let seen = stop(wire);
    assert_eq!(seen.origin.len(), 6, "a miss for each of six keys, once");
    assert_eq!(seen.lane, 6, "and the second round is the lane's");
}

const VARIANTS: &[Step] = &[
    Step::Reply(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: max-age=60\r\nVary: Accept-Language\r\n\r\nen"),
    Step::Reply(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: max-age=60\r\nVary: Accept-Language\r\n\r\nfr"),
    Step::Reply(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nCache-Control: max-age=60\r\nVary: Accept-Language\r\n\r\nde"),
];

#[tokio::test]
async fn a_hit_is_the_variant_its_request_selects() {
    let wire = wire(Origin::scripted(VARIANTS).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    for (language, answer) in [("en", "en"), ("fr", "fr"), ("de", "de")] {
        let reply = client
            .get("/v", &format!("Accept-Language: {language}\r\n"))
            .await;
        assert_eq!(reply.body, answer.as_bytes());
    }
    for _ in 0..3 {
        for (language, answer) in [("en", "en"), ("de", "de"), ("fr", "fr")] {
            let reply = client
                .get("/v", &format!("Accept-Language: {language}\r\n"))
                .await;
            assert_eq!(reply.body, answer.as_bytes(), "the variant of {language}");
        }
    }
    let absent = client.get("/v", "").await;
    assert_eq!(absent.status(), "HTTP/1.1 200 OK");
    let seen = stop(wire);
    assert_eq!(
        seen.origin.len(),
        4,
        "three variants and a request with none of the field"
    );
    assert_eq!(seen.lane, 9);
}

#[tokio::test]
async fn an_entry_that_must_be_revalidated_is_never_a_hit_of_the_lane() {
    for fields in [
        "Cache-Control: max-age=0\r\nETag: \"v1\"\r\n",
        "Cache-Control: max-age=60, no-cache\r\nETag: \"v1\"\r\n",
        "Cache-Control: max-age=0, stale-while-revalidate=60\r\n",
        "Cache-Control: no-store\r\n",
        "Cache-Control: private, max-age=60\r\n",
    ] {
        let wire = wire(Origin::start(reply(fields, b"ok")).await, |_| {}).await;
        let mut client = Client::connect(wire.proxy).await;
        for _ in 0..4 {
            let reply = client.get("/item", "").await;
            assert_eq!(reply.status(), "HTTP/1.1 200 OK", "{fields}");
            assert_eq!(reply.body, b"ok", "{fields}");
        }
        assert_eq!(stop(wire).lane, 0, "{fields}");
    }
}

#[tokio::test]
async fn a_connection_that_asks_to_close_is_answered_and_closed() {
    let wire = wire(Origin::start(reply(FRESH, b"ok")).await, |_| {}).await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    let mut closing = Client::connect(wire.proxy).await;
    let hit = closing.get("/item", "Connection: close\r\n").await;
    assert_eq!(hit.field("connection"), Some("close"));
    assert_eq!(hit.body, b"ok");
    assert!(closing.closed().await);
    assert_eq!(stop(wire).lane, 1);
}

fn purgeable(token_file: std::path::PathBuf) -> impl FnOnce(&mut ConfigDraft) {
    move |config| {
        let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
        else {
            unreachable!("the route proxies");
        };
        let cache = policy.cache.as_mut().expect("cache policy");
        cache.purge_authorization = Some(CachePurgeAuthorization::BearerTokenFile {
            token_file_path: token_file,
        });
    }
}

#[tokio::test]
async fn a_purged_entry_is_not_served_again_until_the_cache_is_refilled() {
    let directory = tempfile::tempdir().expect("token directory");
    let token = "t".repeat(40);
    let token_file = process_support::write_token(directory.path(), &token, 0o600);
    let wire = wire(
        Origin::start(reply(FRESH, b"ok")).await,
        purgeable(token_file),
    )
    .await;
    let mut client = Client::connect(wire.proxy).await;
    client.get("/item", "").await;
    assert_eq!(client.get("/item", "").await.field("age"), Some("0"));
    let refused = client.request("PURGE", "/item", "").await;
    assert_eq!(refused.status(), "HTTP/1.1 401 Unauthorized");
    assert_eq!(client.get("/item", "").await.status(), "HTTP/1.1 200 OK");
    let purged = client
        .request(
            "PURGE",
            "/item",
            &format!("Authorization: Bearer {token}\r\n"),
        )
        .await;
    assert_eq!(purged.status(), "HTTP/1.1 200 OK");
    let refilled = client.get("/item", "").await;
    assert_eq!(
        refilled.field("age"),
        None,
        "the miss after a purge comes from the origin"
    );
    assert_eq!(client.get("/item", "").await.field("age"), Some("0"));
    let missing = client
        .request(
            "PURGE",
            "/nothing",
            &format!("Authorization: Bearer {token}\r\n"),
        )
        .await;
    assert_eq!(missing.status(), "HTTP/1.1 404 Not Found");
    let seen = stop(wire);
    assert_eq!(seen.origin.len(), 2, "filled, purged, filled again");
    assert_eq!(seen.lane, 3);
}

#[tokio::test]
async fn hits_on_many_connections_over_two_workers_all_come_from_the_cache() {
    const CLIENTS: usize = 8;
    const ROUNDS: usize = 40;
    let payload = body(1024);
    let wire = wire(Origin::start(reply(FRESH, &payload)).await, |config| {
        config.listener_workers = Some(2);
    })
    .await;
    let mut warm = Client::connect(wire.proxy).await;
    warm.get("/item", "").await;
    warm.get("/item?", "").await;
    let proxy = wire.proxy;
    let clients = (0..CLIENTS)
        .map(|index| {
            let payload = payload.clone();
            tokio::spawn(async move {
                let mut client = Client::connect(proxy).await;
                for _ in 0..ROUNDS {
                    let path = if index % 2 == 0 { "/item" } else { "/item?" };
                    let hit = client.get(path, "").await;
                    assert_eq!(hit.status(), "HTTP/1.1 200 OK");
                    if index % 2 == 0 {
                        assert_eq!(hit.body, payload);
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    for client in clients {
        client.await.expect("a client served by hits");
    }
    let seen = stop(wire);
    assert_eq!(seen.origin.len(), 2, "one fill per key, then hits");
    assert_eq!(seen.lane, CLIENTS * ROUNDS);
}
