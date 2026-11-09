#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use oxiroute_config::{DownstreamTimeoutPolicy, Stats, StatsPage, StatsPageAdminPolicy};

use config_support::empty_config;
use http_support::{http_request, raw_http_request};
use process_support::{ServerProcess, reserve_tcp_address};

const POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                      img-src 'self' data:; font-src 'self'; form-action 'self'; base-uri 'none'; \
                      frame-ancestors 'none'";

#[tokio::test]
async fn the_page_listener_serves_its_script_as_an_immutable_validated_asset_and_nothing_nearby() {
    let address = reserve_tcp_address();
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: address,
            uri_prefix: "/haproxy".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
    });
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        None,
    );
    let _bound = server.connect_when_bound(address).await;

    let shell = http_request(address, "GET", "/haproxy", &[], &[]).await;
    assert_eq!(shell.header("cache-control"), Some("no-store"));
    assert_eq!(shell.header("content-security-policy"), Some(POLICY));
    let html = shell.text();
    let script = html
        .split("<script")
        .nth(1)
        .and_then(|tag| tag.split("src=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .expect("the shell loads a script file");
    assert!(script.starts_with("/haproxy/assets/"), "{script}");

    let asset = http_request(address, "GET", script, &[], &[]).await;
    assert_eq!(asset.status, 200);
    assert_eq!(
        asset.header("content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        asset.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(asset.header("content-security-policy"), Some(POLICY));
    assert_eq!(asset.header("x-content-type-options"), Some("nosniff"));
    assert!(!asset.body().is_empty());
    let etag = asset.header("etag").expect("asset validator").to_owned();

    let head = http_request(address, "HEAD", script, &[], &[]).await;
    assert!(head.body().is_empty());
    assert_eq!(
        head.header("content-length"),
        asset.header("content-length")
    );
    assert_eq!(head.header("etag"), Some(etag.as_str()));

    let revalidated = http_request(address, "GET", script, &[("If-None-Match", &etag)], &[]).await;
    assert_eq!(revalidated.status, 304);
    assert!(revalidated.body().is_empty());
    assert_eq!(revalidated.header("etag"), Some(etag.as_str()));
    assert_eq!(
        revalidated.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );

    for path in [
        "/haproxy/assets/",
        "/haproxy/assets/missing-12345678.js",
        "/haproxy/assets/../index.html",
        "/haproxy/assets/%2e%2e/index.html",
        "/haproxy/assets/..%2f..%2fCargo.toml",
        "/haproxy/index.html",
        "/assets/missing-12345678.js",
    ] {
        let response = http_request(address, "GET", path, &[], &[]).await;
        assert_eq!(response.status, 404, "{path}");
        assert_eq!(response.header("cache-control"), Some("no-store"), "{path}");
    }
    let post = http_request(address, "POST", script, &[], b"").await;
    assert_eq!(post.status, 405);

    server.shutdown();
}
