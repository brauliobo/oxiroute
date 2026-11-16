use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use oxiroute_import::squid::import;
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

use crate::{common::exchange, process_support};

#[tokio::test]
async fn imported_squid_candidate_serves_authenticated_http_over_daemon() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = origin.accept().await.expect("origin accept");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.expect("origin read");
                assert_ne!(read, 0, "origin request ended before headers");
                request.extend_from_slice(&buffer[..read]);
            }
            assert!(request.starts_with(b"GET /imported HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .expect("origin response");
        }
    });

    let directory = tempdir().expect("Squid import directory");
    let htpasswd = directory.path().join("proxy.htpasswd");
    fs::write(&htpasswd, "myname:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n").expect("write htpasswd");
    fs::set_permissions(&htpasswd, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    let proxy_address = process_support::reserve_tcp_address();
    let squid = directory.path().join("squid.conf");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/synthetic/squid/forward-proxy-auth.conf");
    let source = fs::read_to_string(fixture)
        .expect("read audited Squid fixture")
        .replace("http_port 31280", &format!("http_port {proxy_address}"))
        // the test origin listens on loopback, which the stock `to_localhost` guard would deny
        .replace("http_access deny to_localhost\n", "")
        .replace("/path/to/htpasswd", &htpasswd.display().to_string());
    fs::write(&squid, source).expect("write Squid source");
    let report = import(&squid);
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::to_draft)
        .expect("finalized imported Squid candidate");
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let missing = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/imported HTTP/1.1\r\nHost: stale\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(missing.starts_with(b"HTTP/1.1 200"));
    let accepted = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/imported HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(accepted.starts_with(b"HTTP/1.1 200"));
    assert!(accepted.ends_with(b"ok"));

    origin_task.await.expect("origin task");
    server.shutdown();
}

#[tokio::test]
async fn imported_squid_authentication_rejects_missing_credentials() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("origin accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("origin read");
            assert_ne!(read, 0, "origin request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /authenticated HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("origin response");
    });

    let directory = tempdir().expect("Squid import directory");
    let htpasswd = directory.path().join("proxy.htpasswd");
    fs::write(&htpasswd, "myname:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n").expect("write htpasswd");
    fs::set_permissions(&htpasswd, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    let proxy_address = process_support::reserve_tcp_address();
    let squid = directory.path().join("squid.conf");
    fs::write(
        &squid,
        format!(
            "http_port {proxy_address}\naccess_log none\nacl allowed port {}\nhttp_access deny CONNECT !allowed\nauth_param basic program /usr/lib/squid/basic_ncsa_auth {}\nauth_param basic realm Imported proxy\nauth_param basic credentialsttl 2 hours\nacl users proxy_auth REQUIRED\nhttp_access allow users\nhttp_access deny all\nforwarded_for delete\nvia off\n",
            origin_address.port(),
            htpasswd.display()
        ),
    )
    .expect("write Squid source");
    let report = import(&squid);
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::to_draft)
        .expect("finalized authenticated Squid candidate");
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let missing = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/authenticated HTTP/1.1\r\nHost: stale\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(missing.starts_with(b"HTTP/1.1 407"));
    let accepted = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/authenticated HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(accepted.starts_with(b"HTTP/1.1 200"));

    origin_task.await.expect("origin task");
    server.shutdown();
}
