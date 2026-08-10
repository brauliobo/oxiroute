#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, path::Path};

use http_support::raw_http_request;
use oxiroute_config::{load_lua, render_lua};
use oxiroute_import::varnish::{LoweringStatus, VarnishdInvocation, import};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

#[tokio::test]
async fn imported_varnish_candidate_serves_and_caches_http_on_a_real_listener() {
    let origin = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Varnish origin bind");
    let origin_address = origin.local_addr().expect("Varnish origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("Varnish origin accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("Varnish origin read");
            assert_ne!(read, 0, "Varnish request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /cache-hit HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nvarnish")
            .await
            .expect("Varnish origin response");
    });

    let directory = tempdir().expect("Varnish import directory");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/varnish/exact.vcl");
    let source = fs::read_to_string(fixture)
        .expect("read exact Varnish fixture")
        .replace("127.0.0.1", &origin_address.ip().to_string())
        .replacen("8080", &origin_address.port().to_string(), 1);
    let root = directory.path().join("exact.vcl");
    fs::write(&root, source).expect("write runtime Varnish fixture");

    let proxy_address = process_support::reserve_tcp_address();
    let report = import(
        &root,
        &VarnishdInvocation::new([
            "varnishd",
            "-a",
            &proxy_address.to_string(),
            "-s",
            "cache=malloc,16M",
            "-p",
            "default_ttl=120s",
            "-p",
            "default_grace=10s",
            "-p",
            "default_keep=300s",
            "-F",
        ]),
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(report.lowering, LoweringStatus::Lowered);
    let config = report
        .candidate
        .into_config()
        .expect("finalized Varnish candidate");
    let rendered = render_lua(&config).expect("render finalized Varnish candidate");
    load_lua(&rendered).expect("decode rendered Varnish candidate");

    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;
    let request =
        b"GET /cache-hit HTTP/1.1\r\nHost: cache.example.test\r\nConnection: close\r\n\r\n";

    let first = raw_http_request(proxy_address, request).await;
    assert_eq!(first.status, 200);
    assert_eq!(first.header("x-cache"), Some("hit"));
    assert_eq!(first.body(), b"varnish");

    let second = raw_http_request(proxy_address, request).await;
    assert_eq!(second.status, 200);
    assert_eq!(second.header("x-cache"), Some("hit"));
    assert_eq!(second.body(), b"varnish");

    origin_task.await.expect("Varnish origin task");
    server.shutdown();
}
