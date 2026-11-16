#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/h1_wire.rs"]
mod h1_wire;
mod origin;
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/revalidating_origin.rs"]
mod revalidating_origin;

use std::{
    fmt::Write as _,
    io::Read as _,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use h1_wire::{Origin, Wire, find};
use origin::{BODY_BYTES, CompressionOrigin, hex_text, html, noise};
use oxiroute_config::{
    CacheCompressionCoding, CacheStore, ConfigDraft, HttpCacheCompression, HttpRouteAction,
};
use revalidating_origin::with_cache;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn encoding(&self) -> Option<&str> {
        self.header("content-encoding")
    }

    fn varies_on_accept_encoding(&self) -> bool {
        self.headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("vary"))
            .any(|(_, value)| value.to_ascii_lowercase().contains("accept-encoding"))
    }

    fn decoded(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self.encoding() {
            None => out.clone_from(&self.body),
            Some("gzip") => {
                flate2::read::GzDecoder::new(&self.body[..])
                    .read_to_end(&mut out)
                    .expect("gzip body");
            }
            Some("br") => {
                brotli::Decompressor::new(&self.body[..], 4096)
                    .read_to_end(&mut out)
                    .expect("brotli body");
            }
            Some("zstd") => out = zstd::decode_all(&self.body[..]).expect("zstd body"),
            Some(other) => panic!("unexpected coding {other}"),
        }
        out
    }
}

async fn send(proxy: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)]) -> Reply {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: cache.test\r\nConnection: close\r\n");
    for (name, value) in headers {
        let _ = write!(request, "{name}: {value}\r\n");
    }
    request.push_str("\r\n");
    let mut stream = TcpStream::connect(proxy).await.expect("connect");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut bytes = Vec::new();
    timeout(Duration::from_secs(10), stream.read_to_end(&mut bytes))
        .await
        .expect("response")
        .expect("read");
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> Reply {
    let end = find(bytes, b"\r\n\r\n").expect("response head") + 4;
    let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: bytes[end..].to_vec(),
    }
}

fn with_compression(config: &mut ConfigDraft, edit: impl FnOnce(&mut HttpCacheCompression)) {
    with_cache(config);
    let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
    else {
        unreachable!("wire route is a proxy route");
    };
    let mut compression = HttpCacheCompression::default();
    edit(&mut compression);
    policy.cache.as_mut().expect("cache policy").compression = Some(compression);
}

struct Harness {
    origin: CompressionOrigin,
    wire: Wire,
}

impl Harness {
    async fn start(edit: impl FnOnce(&mut ConfigDraft) + 'static) -> Self {
        let origin = CompressionOrigin::start().await;
        let wire = Wire::over(
            Origin {
                address: origin.address,
                requests: Arc::default(),
            },
            edit,
        )
        .await;
        Self { origin, wire }
    }

    async fn default() -> Self {
        Self::start(|config| with_compression(config, |_| {})).await
    }

    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> Reply {
        send(self.wire.proxy, "GET", path, headers).await
    }

    async fn warm(&self, path: &str) -> Reply {
        let reply = self.get(path, &[]).await;
        assert_eq!(reply.status, 200, "{path}");
        reply
    }

    fn finish(self) {
        self.wire.finish();
        self.origin.finish();
    }
}

#[tokio::test]
async fn negotiation_matrix_serves_the_best_coding_and_decodes_to_the_origin_body() {
    let harness = Harness::default().await;
    harness.warm("/page.html").await;
    let cases: &[(Option<&str>, Option<&str>)] = &[
        (None, None),
        (Some(""), None),
        (Some("identity"), None),
        (Some("deflate"), None),
        (Some("gzip"), Some("gzip")),
        (Some("GZIP"), Some("gzip")),
        (Some("x-gzip"), Some("gzip")),
        (Some("br"), Some("br")),
        (Some("zstd"), Some("zstd")),
        (Some("gzip, deflate, br, zstd"), Some("zstd")),
        (Some("gzip, deflate, br"), Some("br")),
        (Some("gzip;q=0.9, br;q=0.8"), Some("gzip")),
        (Some("gzip;q=0, br"), Some("br")),
        (Some("*"), Some("zstd")),
        (Some("*;q=0, gzip"), Some("gzip")),
        (Some("br;q=0, gzip;q=0, zstd;q=0"), None),
        (Some("identity;q=1, gzip;q=0.5"), None),
    ];
    for (accept, expected) in cases {
        let headers: Vec<(&str, &str)> = accept
            .iter()
            .map(|value| ("Accept-Encoding", *value))
            .collect();
        let reply = harness.get("/page.html", &headers).await;
        assert_eq!(reply.status, 200, "{accept:?}");
        assert_eq!(reply.encoding(), *expected, "Accept-Encoding: {accept:?}");
        assert!(
            reply.varies_on_accept_encoding(),
            "{accept:?}: {:?}",
            reply.headers
        );
        assert_eq!(reply.decoded(), html(), "{accept:?}");
        let length: usize = reply
            .header("content-length")
            .expect("length")
            .parse()
            .expect("number");
        assert_eq!(length, reply.body.len(), "{accept:?}");
        if expected.is_some() {
            assert!(reply.body.len() < html().len(), "{accept:?}");
        }
    }
    assert_eq!(harness.origin.requests("/page.html"), 1);
    harness.finish();
}

#[tokio::test]
async fn each_coding_is_built_once_and_served_identically_afterwards() {
    let harness = Harness::default().await;
    harness.warm("/page.html").await;
    for coding in ["gzip", "br", "zstd"] {
        let first = harness
            .get("/page.html", &[("Accept-Encoding", coding)])
            .await;
        let second = harness
            .get("/page.html", &[("Accept-Encoding", coding)])
            .await;
        assert_eq!(first.encoding(), Some(coding));
        assert_eq!(first.body, second.body, "{coding}");
        assert_eq!(first.header("etag"), second.header("etag"), "{coding}");
    }
    assert_eq!(harness.origin.requests("/page.html"), 1);
    harness.finish();
}

#[tokio::test]
async fn coded_representations_carry_a_weak_etag_that_the_origin_still_recognises() {
    let harness = Harness::default().await;
    let plain = harness.warm("/page.html").await;
    assert_eq!(plain.header("etag"), Some("\"v1\""));
    for coding in ["gzip", "br", "zstd"] {
        let coded = harness
            .get("/page.html", &[("Accept-Encoding", coding)])
            .await;
        assert_eq!(coded.header("etag"), Some("W/\"v1\""), "{coding}");
    }
    assert_eq!(harness.origin.requests("/page.html"), 1);
    let matching = harness
        .get(
            "/page.html",
            &[("Accept-Encoding", "br"), ("If-None-Match", "W/\"v1\"")],
        )
        .await;
    assert_eq!(matching.status, 304);
    assert!(matching.body.is_empty());
    assert_eq!(matching.encoding(), None);
    let stale = harness
        .get(
            "/page.html",
            &[("Accept-Encoding", "br"), ("If-None-Match", "\"old\"")],
        )
        .await;
    assert_eq!(stale.status, 200);
    assert_eq!(stale.decoded(), html());
    assert_eq!(harness.origin.requests("/page.html"), 3);
    let cached = harness
        .get("/page.html", &[("Accept-Encoding", "br")])
        .await;
    assert_eq!(cached.encoding(), Some("br"));
    assert_eq!(harness.origin.requests("/page.html"), 3);
    harness.finish();
}

#[tokio::test]
async fn weak_origin_etags_are_kept_and_missing_ones_are_not_invented() {
    let harness = Harness::default().await;
    harness.warm("/weak.html").await;
    let coded = harness
        .get("/weak.html", &[("Accept-Encoding", "gzip")])
        .await;
    assert_eq!(coded.header("etag"), Some("W/\"w1\""));
    harness.warm("/noetag.html").await;
    let without = harness
        .get("/noetag.html", &[("Accept-Encoding", "zstd")])
        .await;
    assert_eq!(without.encoding(), Some("zstd"));
    assert!(without.header("etag").is_none());
    harness.finish();
}

#[tokio::test]
async fn head_describes_the_coded_representation_without_a_body() {
    let harness = Harness::default().await;
    harness.warm("/page.html").await;
    for coding in ["gzip", "br", "zstd"] {
        let get = harness
            .get("/page.html", &[("Accept-Encoding", coding)])
            .await;
        let head = send(
            harness.wire.proxy,
            "HEAD",
            "/page.html",
            &[("Accept-Encoding", coding)],
        )
        .await;
        assert_eq!(head.status, 200, "{coding}");
        assert!(head.body.is_empty(), "{coding}");
        assert_eq!(head.encoding(), Some(coding));
        assert_eq!(
            head.header("content-length"),
            get.header("content-length"),
            "{coding}"
        );
        assert_eq!(head.header("etag"), get.header("etag"), "{coding}");
        assert!(head.varies_on_accept_encoding(), "{coding}");
    }
    harness.finish();
}

#[tokio::test]
async fn incompressible_small_and_already_coded_bodies_are_served_as_stored() {
    let harness = Harness::default().await;
    for (path, varies) in [
        ("/tiny.html", false),
        ("/image.png", false),
        ("/notransform.html", false),
        ("/random.html", true),
    ] {
        harness.warm(path).await;
        let reply = harness
            .get(path, &[("Accept-Encoding", "gzip, br, zstd")])
            .await;
        assert_eq!(reply.status, 200, "{path}");
        assert_eq!(reply.encoding(), None, "{path}");
        assert_eq!(reply.varies_on_accept_encoding(), varies, "{path}");
    }
    let tiny = harness
        .get("/tiny.html", &[("Accept-Encoding", "gzip")])
        .await;
    assert_eq!(tiny.header("etag"), Some("\"tiny\""));
    harness.warm("/pre.html").await;
    let pre = harness
        .get("/pre.html", &[("Accept-Encoding", "gzip, br")])
        .await;
    assert_eq!(pre.encoding(), Some("gzip"));
    assert!(!pre.varies_on_accept_encoding());
    assert_eq!(pre.decoded(), html());
    harness.finish();
}

#[tokio::test]
async fn ranges_are_never_answered_from_a_coded_variant() {
    let harness = Harness::default().await;
    harness.warm("/page.html").await;
    harness
        .get("/page.html", &[("Accept-Encoding", "gzip")])
        .await;
    let ranged = harness
        .get(
            "/page.html",
            &[("Accept-Encoding", "gzip"), ("Range", "bytes=0-9")],
        )
        .await;
    assert_eq!(ranged.encoding(), None);
    assert_eq!(ranged.body.len(), BODY_BYTES);
    harness.finish();
}

#[tokio::test]
async fn configured_codings_and_content_types_bound_the_offer() {
    let harness = Harness::start(|config| {
        with_compression(config, |compression| {
            compression.codings = vec![CacheCompressionCoding::Gzip];
            compression.content_types = vec!["text/plain".into()];
        });
    })
    .await;
    harness.warm("/page.html").await;
    harness.warm("/a.txt").await;
    let page = harness
        .get("/page.html", &[("Accept-Encoding", "zstd, br, gzip")])
        .await;
    assert_eq!(page.encoding(), None);
    assert!(!page.varies_on_accept_encoding());
    let text = harness
        .get("/a.txt", &[("Accept-Encoding", "zstd, br, gzip")])
        .await;
    assert_eq!(text.encoding(), Some("gzip"));
    assert_eq!(text.decoded(), hex_text(1));
    let refused = harness
        .get("/a.txt", &[("Accept-Encoding", "zstd, br")])
        .await;
    assert_eq!(refused.encoding(), None);
    harness.finish();
}

#[tokio::test]
async fn variants_are_charged_to_the_cache_bound_and_evict_the_oldest_entry() {
    let harness = Harness::start(|config| {
        with_compression(config, |_| {});
        config.cache_stores = vec![CacheStore::memory("memory", 27_000)];
    })
    .await;
    for path in ["/a.txt", "/b.txt", "/c.txt"] {
        harness.warm(path).await;
    }
    let coded = harness.get("/c.txt", &[("Accept-Encoding", "gzip")]).await;
    assert_eq!(coded.encoding(), Some("gzip"));
    assert_eq!(coded.decoded(), hex_text(3));
    for path in ["/b.txt", "/c.txt"] {
        harness.get(path, &[]).await;
        assert_eq!(harness.origin.requests(path), 1, "{path} stays cached");
    }
    harness.get("/a.txt", &[]).await;
    assert_eq!(
        harness.origin.requests("/a.txt"),
        2,
        "/a.txt was evicted to make room"
    );
    harness.finish();
}
