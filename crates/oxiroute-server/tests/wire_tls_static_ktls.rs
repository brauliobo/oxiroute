#![cfg(target_os = "linux")]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Static files over TLS with `ktls = "static"`: large files leave the page cache for the socket in the kernel, which
//! encrypts them, and every byte, keep-alive request, `KeyUpdate` and close must look like the user-space path.
//!
//! The kernel's `tls` module has to be loaded (`sudo modprobe tls`) for the kernel to be used; without it the profile
//! says so in the log and serves from user space, so these tests still pass and the counter assertions are skipped.

mod support;

use std::{fs, os::unix::fs::PermissionsExt as _};

use oxiroute_config::{
    AlpnProtocol, HttpRouteAction, HttpStaticMimePolicy, HttpStaticPathMapping, HttpVersionPolicy,
    KtlsMode,
};
use pingora::protocols::l4::ktls::probe;
use support::{
    PROXY_SERVER_NAME, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture, h1_request,
    proxy_config, tls_client_config_with_versions, tls_connect, tls_connect_with_config,
};
use tokio::time::timeout;

const BIG: usize = 3 * 1024 * 1024 + 17;

fn pattern(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 7 + index / 251) % 251).expect("below 251"))
        .collect()
}

/// `TlsTxSw`: sockets whose transmit records the kernel encrypted in software, since boot.
fn kernel_tx_sockets() -> u64 {
    fs::read_to_string("/proc/net/tls_stat")
        .ok()
        .and_then(|stat| {
            stat.lines()
                .find_map(|line| line.strip_prefix("TlsTxSw"))
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(0)
}

fn kernel_available() -> bool {
    match probe() {
        Ok(()) => true,
        Err(unavailable) => {
            eprintln!(
                "kernel TLS is not available on this host, user-space path only: {unavailable}"
            );
            false
        }
    }
}

fn serving(root: std::path::PathBuf, ktls: KtlsMode) -> (ProxyHarness, std::net::TcpListener) {
    let unused_origin = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve unused origin");
    let reserved = ReservedListener::new();
    let mut config = proxy_config(
        reserved.address,
        unused_origin.local_addr().expect("unused origin address"),
        vec![AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    config.tls_profiles[0].policy.ktls = ktls;
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
    (
        ProxyHarness::start(&config.validate(), reserved),
        unused_origin,
    )
}

fn static_root(body: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir_in("/dev/shm").expect("static root directory in memory");
    let root = directory.path().join("public");
    fs::create_dir(&root).expect("create static root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure static root");
    fs::write(root.join("big.bin"), body).expect("write the large file");
    fs::write(root.join("small.txt"), b"small").expect("write the small file");
    (directory, root)
}

#[tokio::test]
async fn large_files_arrive_intact_over_kernel_tls_and_the_connection_serves_the_next_request() {
    timeout(TEST_TIMEOUT, async {
        let body = pattern(BIG);
        let (_directory, root) = static_root(&body);
        let (proxy, _origin) = serving(root, KtlsMode::Static);
        let before = kernel_tx_sockets();
        let mut stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("TLS connection to OxiRoute");
        for (index, path) in [
            "/small.txt",
            "/big.bin",
            "/small.txt",
            "/big.bin",
            "/big.bin",
        ]
        .into_iter()
        .enumerate()
        {
            let response = h1_request(&mut stream, path, index == 4)
                .await
                .expect("static response");
            assert_eq!(response.status, 200);
            let expected: &[u8] = if path == "/big.bin" { &body } else { b"small" };
            assert!(response.body == expected, "{path}: body differs");
        }
        if kernel_available() {
            assert!(
                kernel_tx_sockets() > before,
                "the kernel did not encrypt the connection"
            );
        }
        drop(stream);
        proxy.finish().await;
    })
    .await
    .expect("kernel TLS static file test timed out");
}

#[tokio::test]
async fn a_key_update_from_the_client_between_downloads_keeps_the_connection_intact() {
    timeout(TEST_TIMEOUT, async {
        let body = pattern(BIG);
        let (_directory, root) = static_root(&body);
        let (proxy, _origin) = serving(root, KtlsMode::Static);
        let mut stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("TLS connection to OxiRoute");
        for round in 0..3 {
            let response = h1_request(&mut stream, "/big.bin", round == 2)
                .await
                .expect("static response");
            assert!(response.body == body, "round {round}: body differs");
            if round < 2 {
                stream
                    .get_mut()
                    .1
                    .refresh_traffic_keys()
                    .expect("client key update");
            }
        }
        drop(stream);
        proxy.finish().await;
    })
    .await
    .expect("kernel TLS key update test timed out");
}

#[tokio::test]
async fn tls_1_2_clients_get_the_same_files_from_user_space() {
    timeout(TEST_TIMEOUT, async {
        let body = pattern(BIG);
        let (_directory, root) = static_root(&body);
        let (proxy, _origin) = serving(root, KtlsMode::Static);
        let config = tls_client_config_with_versions(
            &fixture("ca-a.pem"),
            &[b"http/1.1"],
            &[&rustls::version::TLS12],
        )
        .expect("TLS 1.2 client");
        let mut stream = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, config)
            .await
            .expect("TLS 1.2 connection to OxiRoute");
        for round in 0..2 {
            let response = h1_request(&mut stream, "/big.bin", round == 1)
                .await
                .expect("static response");
            assert!(response.body == body, "round {round}: body differs");
        }
        drop(stream);
        proxy.finish().await;
    })
    .await
    .expect("TLS 1.2 static file test timed out");
}

#[tokio::test]
async fn a_profile_without_the_option_serves_the_same_bytes_from_user_space() {
    timeout(TEST_TIMEOUT, async {
        let body = pattern(BIG);
        let (_directory, root) = static_root(&body);
        let (proxy, _origin) = serving(root, KtlsMode::Off);
        let mut stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("TLS connection to OxiRoute");
        let response = h1_request(&mut stream, "/big.bin", true)
            .await
            .expect("static response");
        assert!(response.body == body);
        drop(stream);
        proxy.finish().await;
    })
    .await
    .expect("unconfigured profile test timed out");
}
