//! Fixed responses, redirects, route selection and route access policies.

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{
    client::Req,
    config::{API_KEY, BEARER_TOKEN},
    context::Context,
};

pub async fn run(c: &mut Context) {
    fixed(c).await;
    redirects(c).await;
    routing(c).await;
    access(c).await;
}

async fn fixed(c: &mut Context) {
    c.run("fixed/get-ok", vec![Req::get("/fixed/ok")]).await;
    c.run("fixed/head-ok", vec![Req::new("HEAD", "/fixed/ok")])
        .await;
    c.run(
        "fixed/post-body-ignored",
        vec![Req::new("POST", "/fixed/ok").text("ignored")],
    )
    .await;
    c.run("fixed/query-ignored", vec![Req::get("/fixed/ok?a=1&b=2")])
        .await;
    c.run(
        "fixed/three-on-one-connection",
        vec![
            Req::get("/fixed/ok"),
            Req::get("/fixed/empty"),
            Req::get("/fixed/404"),
        ],
    )
    .await;
    c.run("fixed/empty-body", vec![Req::get("/fixed/empty")])
        .await;
    c.run(
        "fixed/empty-body-head",
        vec![Req::new("HEAD", "/fixed/empty")],
    )
    .await;
    for status in ["204", "205", "304"] {
        c.run(
            &format!("fixed/status-{status}-without-body"),
            vec![Req::get(&format!("/fixed/{status}"))],
        )
        .await;
        c.run(
            &format!("fixed/status-{status}-head"),
            vec![Req::new("HEAD", &format!("/fixed/{status}"))],
        )
        .await;
    }
    c.run("fixed/status-201-location", vec![Req::get("/fixed/201")])
        .await;
    c.run("fixed/status-404", vec![Req::get("/fixed/404")])
        .await;
    c.run("fixed/status-503-retry-after", vec![Req::get("/fixed/503")])
        .await;
    c.run(
        "fixed/duplicate-headers-are-kept",
        vec![Req::get("/fixed/duplicates")],
    )
    .await;
    c.run("fixed/utf8-body", vec![Req::get("/fixed/utf8")])
        .await;
    c.run("fixed/large-body", vec![Req::get("/fixed/large")])
        .await;
    c.run(
        "fixed/large-body-head",
        vec![Req::new("HEAD", "/fixed/large")],
    )
    .await;
    c.run(
        "fixed/custom-method",
        vec![
            Req::new("PURGE", "/fixed/ok"),
            Req::new("OPTIONS", "/fixed/ok"),
        ],
    )
    .await;
}

async fn redirects(c: &mut Context) {
    c.run("redirect/literal-301", vec![Req::get("/redir/301")])
        .await;
    c.run(
        "redirect/literal-head",
        vec![Req::new("HEAD", "/redir/301")],
    )
    .await;
    c.run(
        "redirect/template-302-extra-header",
        vec![Req::get("/redir/302")],
    )
    .await;
    c.run(
        "redirect/template-308-keeps-the-query",
        vec![Req::get("/redir/308?next=1&x=%20y")],
    )
    .await;
    c.run(
        "redirect/template-308-head",
        vec![Req::new("HEAD", "/redir/308?next=1")],
    )
    .await;
    c.run(
        "redirect/template-host-is-normalised",
        vec![Req::get("/redir/308").host("Mixed.Example.:8443")],
    )
    .await;
    c.run(
        "redirect/template-post",
        vec![Req::new("POST", "/redir/302").text("body")],
    )
    .await;
}

async fn routing(c: &mut Context) {
    c.run("routing/not-found", vec![Req::get("/nothing-here")])
        .await;
    c.run(
        "routing/not-found-head",
        vec![Req::new("HEAD", "/nothing-here")],
    )
    .await;
    c.run(
        "routing/exact-does-not-match-a-longer-path",
        vec![Req::get("/fixed/ok/extra")],
    )
    .await;
    c.run("routing/segment-prefix-boundary", vec![Req::get("/pxyz")])
        .await;
    c.run(
        "routing/host-match",
        vec![Req::get("/hosted").host("only.example")],
    )
    .await;
    c.run("routing/host-mismatch", vec![Req::get("/hosted")])
        .await;
    c.run("routing/method-match", vec![Req::new("POST", "/post-only")])
        .await;
    c.run("routing/method-mismatch", vec![Req::get("/post-only")])
        .await;
}

async fn access(c: &mut Context) {
    let bearer = format!("Bearer {BEARER_TOKEN}");
    let basic = |credentials: &str| format!("Basic {}", STANDARD.encode(credentials));
    c.run("auth/bearer-missing", vec![Req::get("/auth/bearer")])
        .await;
    c.run(
        "auth/bearer-missing-head",
        vec![Req::new("HEAD", "/auth/bearer")],
    )
    .await;
    c.run(
        "auth/bearer-valid",
        vec![Req::get("/auth/bearer").header("authorization", &bearer)],
    )
    .await;
    c.run(
        "auth/bearer-wrong",
        vec![Req::get("/auth/bearer").header("authorization", "Bearer wrong")],
    )
    .await;
    c.run(
        "auth/bearer-duplicated-header",
        vec![
            Req::get("/auth/bearer")
                .header("authorization", &bearer)
                .header("authorization", &bearer),
        ],
    )
    .await;
    c.run(
        "auth/api-key-valid",
        vec![Req::get("/auth/api-key").header("x-api-key", &format!("Bearer {API_KEY}"))],
    )
    .await;
    c.run("auth/api-key-missing", vec![Req::get("/auth/api-key")])
        .await;
    c.run(
        "auth/basic-valid",
        vec![Req::get("/auth/basic").header("authorization", &basic("alice:secret"))],
    )
    .await;
    c.run(
        "auth/basic-wrong-password",
        vec![Req::get("/auth/basic").header("authorization", &basic("alice:nope"))],
    )
    .await;
    c.run("auth/basic-missing", vec![Req::get("/auth/basic")])
        .await;
    c.run(
        "auth/proxy-denied-without-contacting-the-origin",
        vec![Req::get("/auth/proxy")],
    )
    .await;
    c.run(
        "auth/proxy-allowed",
        vec![Req::get("/auth/proxy").header("authorization", &bearer)],
    )
    .await;
}
