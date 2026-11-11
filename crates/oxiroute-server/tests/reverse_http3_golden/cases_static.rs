//! Static files: hits, misses, conditional requests, ranges, indexes, error documents and the body bound.

use serde_json::Value;

use crate::{
    client::Req,
    context::{Context, host},
};

const MODIFIED: &str = "Tue, 14 Nov 2023 22:13:20 GMT";
const BEFORE: &str = "Mon, 13 Nov 2023 22:13:20 GMT";
const AFTER: &str = "Wed, 15 Nov 2023 22:13:20 GMT";

pub async fn run(c: &mut Context) {
    plain(c).await;
    conditional(c).await;
    ranges(c).await;
    layouts(c).await;
}

fn header(outcome: &Value, name: &str) -> String {
    let headers = outcome["headers"].as_array().expect("response headers");
    headers
        .iter()
        .find(|pair| pair[0] == name)
        .and_then(|pair| pair[1].as_str())
        .expect("validator header")
        .to_owned()
}

/// A first request on a connection, then requests built from its validators.
async fn after_first(
    c: &mut Context,
    id: &str,
    path: &str,
    then: impl FnOnce(&str, &str) -> Vec<Req>,
) {
    let host = host(id);
    let mut session = c.session().await;
    let first = c.step(&mut session, &host, &Req::get(path)).await;
    let (etag, modified) = (
        header(&first["outcome"], "etag"),
        header(&first["outcome"], "last-modified"),
    );
    let mut steps = vec![first];
    for request in then(&etag, &modified) {
        steps.push(c.step(&mut session, &host, &request).await);
    }
    steps.last_mut().expect("steps")["connection_after"] = session.state().await;
    session.close();
    c.record(id, vec![host], steps, None);
}

async fn plain(c: &mut Context) {
    c.run("static/hit", vec![Req::get("/static/ok.txt")]).await;
    c.run("static/head", vec![Req::new("HEAD", "/static/ok.txt")])
        .await;
    c.run("static/miss", vec![Req::get("/static/missing.txt")])
        .await;
    c.run(
        "static/miss-head",
        vec![Req::new("HEAD", "/static/missing.txt")],
    )
    .await;
    c.run(
        "static/empty-file",
        vec![
            Req::get("/static/empty.txt"),
            Req::new("HEAD", "/static/empty.txt"),
        ],
    )
    .await;
    c.run(
        "static/mime-types",
        vec![
            Req::get("/static/page.html"),
            Req::get("/static/data.json"),
            Req::get("/static/big.bin"),
        ],
    )
    .await;
    c.run("static/nested", vec![Req::get("/static/sub/inner.txt")])
        .await;
    c.run(
        "static/large-file",
        vec![
            Req::get("/static/big.bin"),
            Req::new("HEAD", "/static/big.bin"),
        ],
    )
    .await;
    c.run(
        "static/over-the-body-bound",
        vec![
            Req::get("/static/oversized.bin"),
            Req::new("HEAD", "/static/oversized.bin"),
        ],
    )
    .await;
    c.run(
        "static/method-not-allowed",
        vec![
            Req::new("POST", "/static/ok.txt").text("x"),
            Req::new("OPTIONS", "/static/ok.txt"),
        ],
    )
    .await;
    c.run(
        "static/trailing-slash-on-a-file",
        vec![Req::get("/static/ok.txt/")],
    )
    .await;
    c.run("static/query-ignored", vec![Req::get("/static/ok.txt?v=1")])
        .await;
    c.run(
        "static/without-etag",
        vec![
            Req::get("/noetag/ok.txt"),
            Req::new("HEAD", "/noetag/ok.txt"),
        ],
    )
    .await;
    c.run(
        "static/custom-header-on-every-response",
        vec![Req::get("/static/ok.txt"), Req::get("/static/missing.txt")],
    )
    .await;
}

async fn conditional(c: &mut Context) {
    after_first(
        c,
        "static/if-none-match-hit",
        "/static/ok.txt",
        |etag, _| {
            vec![
                Req::get("/static/ok.txt").header("if-none-match", etag),
                Req::new("HEAD", "/static/ok.txt").header("if-none-match", etag),
            ]
        },
    )
    .await;
    after_first(
        c,
        "static/if-none-match-variants",
        "/static/ok.txt",
        |etag, _| {
            let weak = format!("W/{etag}");
            let list = format!("\"other\", {etag}");
            ["*", "\"stale\"", &weak, &list]
                .into_iter()
                .map(|value| Req::get("/static/ok.txt").header("if-none-match", value))
                .collect()
        },
    )
    .await;
    after_first(c, "static/if-modified-since", "/static/ok.txt", |_, _| {
        [MODIFIED, BEFORE, AFTER, "not a date"]
            .into_iter()
            .map(|date| Req::get("/static/ok.txt").header("if-modified-since", date))
            .collect()
    })
    .await;
    after_first(c, "static/if-match", "/static/ok.txt", |etag, _| {
        vec![
            Req::get("/static/ok.txt").header("if-match", etag),
            Req::get("/static/ok.txt").header("if-match", "\"stale\""),
            Req::get("/static/ok.txt").header("if-match", "*"),
        ]
    })
    .await;
    after_first(c, "static/if-unmodified-since", "/static/ok.txt", |_, _| {
        [BEFORE, MODIFIED, AFTER]
            .into_iter()
            .map(|date| Req::get("/static/ok.txt").header("if-unmodified-since", date))
            .collect()
    })
    .await;
    after_first(
        c,
        "static/if-none-match-wins-over-if-modified-since",
        "/static/ok.txt",
        |_, _| {
            vec![
                Req::get("/static/ok.txt")
                    .header("if-none-match", "\"stale\"")
                    .header("if-modified-since", AFTER),
            ]
        },
    )
    .await;
    c.run(
        "static/without-etag-if-modified-since",
        vec![
            Req::get("/noetag/ok.txt").header("if-modified-since", AFTER),
            Req::get("/noetag/ok.txt").header("if-none-match", "*"),
        ],
    )
    .await;
}

async fn ranges(c: &mut Context) {
    let range = |path: &str, value: &str| Req::get(path).header("range", value);
    let requests = [
        "bytes=2-5",
        "bytes=-3",
        "bytes=5-",
        "bytes=0-0",
        "bytes=8-100",
        "bytes=0-9",
        "bytes=20-30",
        "bytes=9-5",
    ];
    c.run(
        "static/ranges",
        requests
            .iter()
            .map(|value| range("/static/ok.txt", value))
            .collect(),
    )
    .await;
    let odd = [
        "bytes=abc",
        "items=0-1",
        "bytes=0-1,3-4",
        "bytes=-0",
        "bytes=",
        "bytes=2-5, 7-8",
    ];
    c.run(
        "static/odd-ranges",
        odd.iter()
            .map(|value| range("/static/ok.txt", value))
            .collect(),
    )
    .await;
    c.run(
        "static/range-on-head-and-empty-file",
        vec![
            Req::new("HEAD", "/static/ok.txt").header("range", "bytes=2-5"),
            range("/static/empty.txt", "bytes=0-1"),
            range("/static/empty.txt", "bytes=-1"),
        ],
    )
    .await;
    c.run(
        "static/range-in-a-large-file",
        vec![
            range("/static/big.bin", "bytes=1000-2999"),
            range("/static/big.bin", "bytes=-100"),
        ],
    )
    .await;
    after_first(c, "static/if-range", "/static/ok.txt", |etag, modified| {
        let ranged =
            |validator: &str| range("/static/ok.txt", "bytes=2-5").header("if-range", validator);
        vec![
            ranged(etag),
            ranged("\"stale\""),
            ranged(modified),
            ranged(BEFORE),
        ]
    })
    .await;
}

async fn layouts(c: &mut Context) {
    c.run(
        "static/index-file",
        vec![Req::get("/static/dir/"), Req::new("HEAD", "/static/dir/")],
    )
    .await;
    c.run(
        "static/directory-redirect",
        vec![
            Req::get("/static/dir"),
            Req::new("HEAD", "/static/dir"),
            Req::get("/static/dir?x=1"),
        ],
    )
    .await;
    c.run(
        "static/directory-without-index",
        vec![Req::get("/static/emptydir/"), Req::get("/static/emptydir")],
    )
    .await;
    c.run(
        "static/autoindex",
        vec![
            Req::get("/listing/"),
            Req::new("HEAD", "/listing/"),
            Req::get("/listing/subdir/"),
        ],
    )
    .await;
    c.run(
        "static/spa-fallback",
        vec![
            Req::get("/spa/anything/deep"),
            Req::get("/spa/"),
            Req::get("/spa/index.html"),
        ],
    )
    .await;
    c.run(
        "static/error-document-from-a-file",
        vec![Req::get("/errs/missing"), Req::new("HEAD", "/errs/missing")],
    )
    .await;
    c.run(
        "static/error-document-by-internal-redirect",
        vec![
            Req::get("/errs/emptydir/"),
            Req::new("HEAD", "/errs/emptydir/"),
        ],
    )
    .await;
}
