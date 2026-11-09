#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

mod support;

use std::{fs, os::unix::fs::PermissionsExt as _};

use oxiroute_config::{
    AlpnProtocol, HttpRouteAction, HttpStaticMimePolicy, HttpStaticPathMapping, HttpVersionPolicy,
};
use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, h1_request,
    proxy_config, tls_connect,
};
use tokio::time::timeout;

fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

#[tokio::test]
async fn proxied_bodies_larger_than_a_read_buffer_arrive_intact_on_one_connection() {
    timeout(TEST_TIMEOUT, async {
        let body: &'static [u8] = Box::leak(pattern(300_017).into_boxed_slice());
        let origin = PlainH1Origin::start(body).await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let mut stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("TLS connection to OxiRoute");

        for round in 0..3 {
            let close = round == 2;
            let response = h1_request(&mut stream, "/large", close)
                .await
                .expect("proxied large response");
            assert_eq!(response.status, 200);
            assert!(response.body == body, "round {round}: body differs");
        }
        origin.wait_for_requests(3).await;

        drop(stream);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("large proxied body test timed out");
}

#[tokio::test]
async fn static_files_larger_than_the_memory_limit_stream_intact_on_one_connection() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("static root directory");
        let root = directory.path().join("public");
        fs::create_dir(&root).expect("create static root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure static root");
        let body = pattern(1_048_576 + 17);
        fs::write(root.join("big.bin"), &body).expect("write large static file");
        fs::write(root.join("small.txt"), b"small").expect("write small static file");

        let unused_origin = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve unused origin");
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            unused_origin.local_addr().expect("unused origin address"),
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.http_services[0].routes[0].action = HttpRouteAction::StaticFiles {
            root_directory: root,
            path_mapping: HttpStaticPathMapping::Root,
            index_files: Vec::new(),
            internal_index_redirects: true,
            directory_redirects: true,
            spa_fallback: None,
            try_files: Vec::new(),
            autoindex: false,
            autoindex_exact_size: true,
            autoindex_local_time: false,
            etag: true,
            mime: HttpStaticMimePolicy::default(),
            headers: Vec::new(),
            error_responses: Vec::new(),
        };
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let mut stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("TLS connection to OxiRoute");

        for (index, path) in ["/big.bin", "/small.txt", "/big.bin"].into_iter().enumerate() {
            let response = h1_request(&mut stream, path, index == 2)
                .await
                .expect("static response");
            assert_eq!(response.status, 200);
            let expected: &[u8] = if path == "/big.bin" { &body } else { b"small" };
            assert!(response.body == expected, "{path}: body differs");
        }

        drop(stream);
        proxy.finish().await;
    })
    .await
    .expect("large static file test timed out");
}
