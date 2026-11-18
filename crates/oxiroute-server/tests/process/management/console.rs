use std::{fs, io::Read as _};

use tempfile::TempDir;

use crate::http_support::{HttpResponse, http_request};
use crate::process_support::{ServerProcess, reserve_tcp_address};
use crate::shared::{TOKEN, management_config};

const SCRIPT_PATH: &str = "/assets/app-abc123.js";

fn console_files() -> (TempDir, String, String) {
    let directory = TempDir::new().expect("console directory");
    fs::create_dir(directory.path().join("assets")).expect("assets directory");
    let index = format!("<!doctype html><script type=\"module\" src=\"{SCRIPT_PATH}\"></script>\n")
        .repeat(40);
    let script = "console.log('console');\n".repeat(200);
    fs::write(directory.path().join("index.html"), &index).expect("index");
    fs::write(
        directory.path().join(SCRIPT_PATH.trim_start_matches('/')),
        &script,
    )
    .expect("script");
    (directory, index, script)
}

fn header<'a>(response: &'a HttpResponse, name: &str) -> &'a str {
    response
        .header(name)
        .unwrap_or_else(|| panic!("{name} is missing: {}", response.text()))
}

fn gunzip(body: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(body)
        .read_to_end(&mut decoded)
        .expect("gzip body");
    decoded
}

fn unbrotli(body: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    brotli::Decompressor::new(body, 4096)
        .read_to_end(&mut decoded)
        .expect("brotli body");
    decoded
}

#[tokio::test]
async fn the_console_is_served_hardened_cacheable_and_revalidated_over_real_tcp() {
    let (directory, index, script) = console_files();
    let management = reserve_tcp_address();
    let config = management_config(management, Some(directory.path().to_path_buf()));
    let mut server = ServerProcess::start(&config.validate().expect("valid config"), Some(TOKEN));
    server.wait_for_tcp(management).await;
    let get = |path: &'static str, headers: Vec<(&'static str, String)>| async move {
        let headers = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect::<Vec<_>>();
        http_request(management, "GET", path, &headers, &[]).await
    };

    let page = get("/", vec![]).await;
    let asset = get(SCRIPT_PATH, vec![]).await;

    for response in [&page, &asset] {
        let policy = header(response, "content-security-policy");
        assert!(policy.contains("default-src 'none'"), "{policy}");
        assert!(policy.contains("script-src 'self'"), "{policy}");
        assert!(policy.contains("media-src 'self'"), "{policy}");
        assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
        assert!(!policy.contains("unsafe"), "{policy}");
        assert_eq!(header(response, "x-content-type-options"), "nosniff");
        assert_eq!(header(response, "referrer-policy"), "no-referrer");
        assert_eq!(header(response, "x-frame-options"), "DENY");
        assert_eq!(header(response, "vary"), "accept-encoding");
    }
    assert_eq!(page.body, index.as_bytes());
    assert_eq!(header(&page, "cache-control"), "no-cache");
    assert_eq!(asset.body, script.as_bytes());
    assert_eq!(
        header(&asset, "cache-control"),
        "public, max-age=31536000, immutable"
    );

    let etag = header(&page, "etag").to_owned();
    let unchanged = get("/", vec![("If-None-Match", etag.clone())]).await;
    assert_eq!(unchanged.status, 304, "{}", unchanged.text());
    assert!(unchanged.body.is_empty());
    assert_eq!(header(&unchanged, "etag"), etag);
    assert_eq!(header(&unchanged, "cache-control"), "no-cache");
    assert!(unchanged.header("content-security-policy").is_some());
    let stale = get("/", vec![("If-None-Match", "\"stale\"".to_owned())]).await;
    assert_eq!(stale.status, 200);

    let gzip = get("/", vec![("Accept-Encoding", "gzip".to_owned())]).await;
    assert_eq!(header(&gzip, "content-encoding"), "gzip");
    assert_eq!(gunzip(&gzip.body), index.as_bytes());
    assert!(gzip.body.len() < index.len());
    let brotli = get("/", vec![("Accept-Encoding", "gzip, br".to_owned())]).await;
    assert_eq!(header(&brotli, "content-encoding"), "br");
    assert_eq!(unbrotli(&brotli.body), index.as_bytes());
    assert_ne!(header(&gzip, "etag"), etag);
    assert_ne!(header(&gzip, "etag"), header(&brotli, "etag"));
    let encoded_unchanged = get(
        "/",
        vec![
            ("Accept-Encoding", "gzip".to_owned()),
            ("If-None-Match", header(&gzip, "etag").to_owned()),
        ],
    )
    .await;
    assert_eq!(encoded_unchanged.status, 304);
    let other_variant = get(
        "/",
        vec![
            ("Accept-Encoding", "gzip".to_owned()),
            ("If-None-Match", etag),
        ],
    )
    .await;
    assert_eq!(other_variant.status, 200);
    server.shutdown();
}
