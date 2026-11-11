//! Proxying to an HTTP/3 origin: what the origin receives, response mapping, header policies and retries.

use serde_json::json;

use crate::{
    client::{Body, Req},
    context::Context,
};

pub async fn run(c: &mut Context) {
    requests(c).await;
    hop_by_hop_requests(c).await;
    policies(c).await;
    responses(c).await;
    failures(c).await;
    retries(c).await;
}

async fn requests(c: &mut Context) {
    c.run("proxy/get", vec![Req::get("/p/echo?a=1&b=two")])
        .await;
    c.run("proxy/head", vec![Req::new("HEAD", "/p/echo")]).await;
    c.run(
        "proxy/post-body",
        vec![Req::new("POST", "/p/echo").text("hello world")],
    )
    .await;
    c.run(
        "proxy/post-with-content-length",
        vec![
            Req::new("POST", "/p/echo")
                .header("content-length", "11")
                .text("hello world"),
        ],
    )
    .await;
    c.run(
        "proxy/post-empty-body",
        vec![Req::new("POST", "/p/echo").header("content-length", "0")],
    )
    .await;
    c.run(
        "proxy/post-without-content-length",
        vec![Req::new("POST", "/p/echo")],
    )
    .await;
    for method in ["PUT", "PATCH", "DELETE", "OPTIONS", "PURGE"] {
        c.run(
            &format!("proxy/method-{}", method.to_ascii_lowercase()),
            vec![Req::new(method, "/p/echo").text("m")],
        )
        .await;
    }
    c.run(
        "proxy/streamed-request-body",
        vec![Req::new("POST", "/p/echo").body(Body::Chunks(vec![
            b"a".repeat(10),
            b"b".repeat(10),
            b"c".repeat(5),
        ]))],
    )
    .await;
    c.run(
        "proxy/request-trailers-are-dropped",
        vec![
            Req::new("POST", "/p/echo")
                .text("body")
                .trailer("x-request-trailer", "t"),
        ],
    )
    .await;
    c.run(
        "proxy/path-and-query-are-kept",
        vec![Req::get("/p/a%20b/c?x=1&y=%2F&z")],
    )
    .await;
    c.run(
        "proxy/request-headers-are-forwarded",
        vec![
            Req::get("/p/echo")
                .header("x-a", "1")
                .header("x-a", "2")
                .header("cookie", "a=1")
                .header("cookie", "b=2")
                .header("user-agent", "golden/1")
                .header("accept-encoding", "gzip, br")
                .header("authorization", "Bearer forwarded")
                .header("x-forwarded-for", "203.0.113.9")
                .header("forwarded", "for=203.0.113.9")
                .header("te", "trailers"),
        ],
    )
    .await;
    c.run(
        "proxy/many-requests-on-one-connection",
        (0..4)
            .map(|index| Req::get(&format!("/p/echo/{index}")))
            .collect(),
    )
    .await;
}

/// Connection-specific fields make a request malformed (RFC 9114 4.2).
async fn hop_by_hop_requests(c: &mut Context) {
    for (name, value) in [
        ("connection", "x-hop"),
        ("keep-alive", "timeout=5"),
        ("proxy-connection", "keep-alive"),
        ("upgrade", "websocket"),
        ("transfer-encoding", "chunked"),
        ("te", "gzip"),
    ] {
        let request = Req::get("/p/echo")
            .header(name, value)
            .header("x-hop", "marker");
        c.run(&format!("proxy/request-field-{name}"), vec![request])
            .await;
    }
}

async fn policies(c: &mut Context) {
    let sources = || {
        Req::get("/pol/x")
            .header("x-source", "one")
            .header("x-source", "two")
            .header("x-remove-me", "gone")
    };
    c.run(
        "policy/request-headers",
        vec![
            sources()
                .header("x-overridden", "client")
                .header("x-forwarded-for", "trusted"),
            Req::get("/pol/y").host("policy-two.policy.test:8443"),
        ],
    )
    .await;
    c.run(
        "policy/request-headers-head-and-post",
        vec![
            Req::new("HEAD", "/pol/x"),
            Req::new("POST", "/pol/x").text("body"),
        ],
    )
    .await;
    c.run(
        "policy/response-headers",
        vec![Req::get("/pol/x").mode("headers")],
    )
    .await;
    c.run(
        "policy/response-headers-on-errors",
        vec![
            Req::get("/pol/x").mode("status-503"),
            Req::get("/pol/x").mode("status-404"),
        ],
    )
    .await;
    c.run(
        "policy/response-headers-head",
        vec![Req::new("HEAD", "/pol/x").mode("headers")],
    )
    .await;
    c.run(
        "policy/upstream-host-is-the-incoming-authority",
        vec![Req::get("/p/echo")],
    )
    .await;
    c.run(
        "policy/upstream-host-literal",
        vec![Req::get("/host-literal/x")],
    )
    .await;
    c.run(
        "policy/upstream-host-endpoint",
        vec![Req::get("/host-endpoint/x")],
    )
    .await;
    c.run(
        "policy/upstream-host-nginx",
        vec![Req::get("/host-nginx/x")],
    )
    .await;
    c.run(
        "policy/path-rewrite",
        vec![Req::get("/rw/a/b?x=1"), Req::get("/rw"), Req::get("/rw/")],
    )
    .await;
}

async fn responses(c: &mut Context) {
    for status in [200, 201, 204, 205, 304, 400, 404, 500, 502, 503] {
        let request = || Req::get("/p/x").mode(&format!("status-{status}"));
        c.run(
            &format!("proxy-response/status-{status}"),
            vec![
                request(),
                Req::new("HEAD", "/p/x").mode(&format!("status-{status}")),
            ],
        )
        .await;
    }
    c.run(
        "proxy-response/headers",
        vec![Req::get("/p/x").mode("headers")],
    )
    .await;
    c.run(
        "proxy-response/hop-by-hop-headers-are-dropped",
        vec![Req::get("/p/x").mode("headers-hop")],
    )
    .await;
    c.run(
        "proxy-response/trailers",
        vec![
            Req::get("/p/x").mode("trailers"),
            Req::new("HEAD", "/p/x").mode("trailers"),
        ],
    )
    .await;
    c.run(
        "proxy-response/trailers-with-hop-by-hop-fields",
        vec![Req::get("/p/x").mode("trailers-hop")],
    )
    .await;
    c.run(
        "proxy-response/large-body",
        vec![
            Req::get("/p/x").mode("big"),
            Req::new("HEAD", "/p/x").mode("big"),
        ],
    )
    .await;
    c.run(
        "proxy-response/no-content-length",
        vec![
            Req::get("/p/x").mode("nolength"),
            Req::new("HEAD", "/p/x").mode("nolength"),
        ],
    )
    .await;
    c.run(
        "proxy-response/empty-body",
        vec![
            Req::get("/p/x").mode("empty"),
            Req::new("HEAD", "/p/x").mode("empty"),
        ],
    )
    .await;
}

async fn failures(c: &mut Context) {
    for mode in ["mismatch", "dupcl", "badcl", "bighdr", "reset"] {
        c.run(
            &format!("proxy-failure/origin-{mode}"),
            vec![Req::get("/p/x").mode(mode), Req::get("/p/x")],
        )
        .await;
    }
    c.run(
        "proxy-failure/origin-hangs",
        vec![Req::get("/p/x").mode("hang"), Req::get("/p/x")],
    )
    .await;
    c.run(
        "proxy-failure/dead-origin",
        vec![Req::get("/dead/x"), Req::new("POST", "/dead/x").text("b")],
    )
    .await;
    c.run(
        "proxy-failure/body-stalls",
        vec![
            Req::new("POST", "/p/x")
                .header("content-length", "10")
                .body(Body::Stall(b"abc".to_vec())),
        ],
    )
    .await;
}

async fn retries(c: &mut Context) {
    c.run(
        "retry/failover-after-a-refused-connection",
        vec![Req::get("/failover/x"), Req::new("HEAD", "/failover/x")],
    )
    .await;
    c.run(
        "retry/post-fails-over-after-a-refused-connection",
        vec![Req::new("POST", "/failover/x").text("body")],
    )
    .await;
    c.run("retry/disabled", vec![Req::get("/no-retry/x")]).await;
    c.run(
        "retry/on-response-status",
        vec![
            Req::get("/retry-status/x")
                .mode("flaky")
                .header("x-flaky-key", "get"),
        ],
    )
    .await;
    c.run(
        "retry/on-response-status-post-passes-through",
        vec![
            Req::new("POST", "/retry-status/x")
                .mode("flaky")
                .header("x-flaky-key", "post")
                .text("b"),
        ],
    )
    .await;
    c.run(
        "retry/on-response-status-exhausted",
        vec![Req::get("/retry-status/x").mode("status-503")],
    )
    .await;
    c.run(
        "retry/other-statuses-are-not-retried",
        vec![Req::get("/retry-status/x").mode("status-500")],
    )
    .await;
}
