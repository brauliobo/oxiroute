//! The response cache: hits without origin contact, revalidation, purging, bypass and stale service.

use crate::{
    client::{Body, Req},
    config::PURGE_TOKEN,
    context::Context,
};

pub async fn run(c: &mut Context) {
    hits(c).await;
    revalidation(c).await;
    purging(c).await;
    request_exceptions(c).await;
    storage_rules(c).await;
}

fn fresh(path: &str) -> Req {
    Req::get(path).mode("cache-fresh")
}

fn purge(path: &str) -> Req {
    Req::new("PURGE", path).header("authorization", &format!("Bearer {PURGE_TOKEN}"))
}

async fn hits(c: &mut Context) {
    c.run(
        "cache/miss-then-hit",
        vec![
            fresh("/c/fresh/one"),
            fresh("/c/fresh/one"),
            fresh("/c/fresh/one"),
        ],
    )
    .await;
    c.run(
        "cache/head-after-get",
        vec![
            fresh("/c/fresh/two"),
            Req::new("HEAD", "/c/fresh/two").mode("cache-fresh"),
        ],
    )
    .await;
    c.run(
        "cache/head-first",
        vec![
            Req::new("HEAD", "/c/fresh/three").mode("cache-fresh"),
            fresh("/c/fresh/three"),
        ],
    )
    .await;
    c.run(
        "cache/query-is-part-of-the-key",
        vec![
            fresh("/c/fresh/q?a=1"),
            fresh("/c/fresh/q?a=2"),
            fresh("/c/fresh/q?a=1"),
        ],
    )
    .await;
    c.run(
        "cache/host-is-part-of-the-key",
        vec![
            fresh("/c/fresh/h"),
            fresh("/c/fresh/h").host("other-host.cache.test"),
        ],
    )
    .await;
    c.run(
        "cache/large-body",
        vec![
            Req::get("/c/fresh/big").mode("big"),
            Req::get("/c/fresh/big").mode("big"),
        ],
    )
    .await;
    c.run(
        "cache/conditional-client-requests",
        vec![
            fresh("/c/fresh/cond"),
            fresh("/c/fresh/cond").header("if-none-match", "\"cond\""),
            fresh("/c/fresh/cond").header("if-none-match", "\"other\""),
            fresh("/c/fresh/cond").header("if-modified-since", "Sun, 02 Jan 2022 00:00:00 GMT"),
            Req::new("HEAD", "/c/fresh/cond")
                .header("if-none-match", "*")
                .mode("cache-fresh"),
        ],
    )
    .await;
}

async fn revalidation(c: &mut Context) {
    let stale = |path: &str| Req::get(path).mode("cache-stale");
    c.run(
        "cache/revalidation",
        vec![
            stale("/c/stale/one"),
            stale("/c/stale/one"),
            stale("/c/stale/one"),
        ],
    )
    .await;
    c.run(
        "cache/no-cache-request",
        vec![
            fresh("/c/fresh/nc"),
            fresh("/c/fresh/nc").header("cache-control", "no-cache"),
            fresh("/c/fresh/nc"),
        ],
    )
    .await;
    c.run(
        "cache/only-if-cached",
        vec![
            fresh("/c/fresh/oic").header("cache-control", "only-if-cached"),
            fresh("/c/fresh/oic"),
            fresh("/c/fresh/oic").header("cache-control", "only-if-cached"),
        ],
    )
    .await;
    c.run(
        "cache/stale-copy-served-when-the-origin-fails",
        vec![
            Req::get("/c/err/one").mode("cache-err"),
            Req::get("/c/err/one").mode("cache-err"),
            Req::get("/c/err/one").mode("cache-err"),
        ],
    )
    .await;
}

async fn purging(c: &mut Context) {
    c.run(
        "cache/purge",
        vec![
            fresh("/c/fresh/p1"),
            purge("/c/fresh/p1"),
            fresh("/c/fresh/p1"),
            purge("/c/fresh/p1"),
            purge("/c/fresh/never-cached"),
        ],
    )
    .await;
    c.run(
        "cache/purge-denied",
        vec![
            fresh("/c/fresh/p2"),
            Req::new("PURGE", "/c/fresh/p2"),
            Req::new("PURGE", "/c/fresh/p2").header("authorization", "Bearer wrong"),
            fresh("/c/fresh/p2"),
        ],
    )
    .await;
    c.run(
        "cache/purge-by-surrogate-tag",
        vec![
            fresh("/c/fresh/t1"),
            fresh("/c/fresh/t2"),
            purge("/c/fresh/ignored").header("surrogate-key", "tag-a"),
            fresh("/c/fresh/t1"),
            purge("/c/fresh/ignored").header("surrogate-key", "no-such-tag"),
        ],
    )
    .await;
    c.run(
        "cache/purge-with-a-query",
        vec![
            fresh("/c/fresh/pq?a=1"),
            purge("/c/fresh/pq?a=1"),
            purge("/c/fresh/pq"),
        ],
    )
    .await;
}

async fn request_exceptions(c: &mut Context) {
    c.run(
        "cache/authorization-bypasses",
        vec![
            fresh("/c/fresh/a1"),
            fresh("/c/fresh/a1").header("authorization", "Bearer user"),
            fresh("/c/fresh/a1"),
        ],
    )
    .await;
    c.run(
        "cache/request-body-bypasses",
        vec![
            fresh("/c/fresh/body"),
            fresh("/c/fresh/body").body(Body::Bytes(b"b".to_vec())),
            fresh("/c/fresh/body"),
        ],
    )
    .await;
}

async fn storage_rules(c: &mut Context) {
    c.run(
        "cache/post-is-not-cached",
        vec![
            Req::new("POST", "/c/fresh/post")
                .mode("cache-fresh")
                .text("a"),
            Req::new("POST", "/c/fresh/post")
                .mode("cache-fresh")
                .text("a"),
        ],
    )
    .await;
    c.run(
        "cache/no-store-response",
        vec![
            Req::get("/c/ns/one").mode("cache-nostore"),
            Req::get("/c/ns/one").mode("cache-nostore"),
        ],
    )
    .await;
    c.run(
        "cache/error-status-is-not-cached",
        vec![
            Req::get("/c/e/one").mode("status-404"),
            Req::get("/c/e/one").mode("status-404"),
        ],
    )
    .await;
    c.run(
        "cache/response-trailers-are-not-cached",
        vec![
            Req::get("/c/t/one").mode("trailers"),
            Req::get("/c/t/one").mode("trailers"),
        ],
    )
    .await;
    c.run(
        "cache/success-without-validators",
        vec![
            Req::get("/c/s/one").mode("echo"),
            Req::get("/c/s/one").mode("echo"),
        ],
    )
    .await;
}
